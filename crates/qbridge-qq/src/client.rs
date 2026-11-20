//! QQ 官方 API v2 的 HTTP 客户端。
//!
//! 所有接口共用 [`Client::request`]：路径模板用于拼接 URL 和记录日志，
//! 日志中不会出现 OpenID。

use crate::{
    error::Error,
    message::{
        GroupInfo, MediaUpload, Member, MemberPage, MuteOp, OutgoingMessage, SentMessage,
        UploadedMedia,
    },
    openid::{GroupOpenId, MemberOpenId, Peer, UserOpenId},
    redact,
};
use futures_util::StreamExt;
use md5::{Digest, Md5};
use reqwest::{
    Client as HttpClient, Method, Response,
    header::{HeaderValue, InvalidHeaderValue},
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha1::Sha1;
use std::{
    fmt::Write as _,
    io::SeekFrom,
    path::Path,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::Mutex,
};
use tracing::Instrument;
use url::Url;

const API_BASE: &str = "https://api.sgroup.qq.com";
const TOKEN_URL: &str = "https://bots.qq.com/app/getAppAccessToken";
const BODY_LIMIT: usize = 1024 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_MARGIN: Duration = Duration::from_secs(60);

const USER_MESSAGES: &str = "/v2/users/{openid}/messages";
const USER_STREAM_MESSAGES: &str = "/v2/users/{openid}/stream_messages";
const GROUP_MESSAGES: &str = "/v2/groups/{openid}/messages";
const USER_FILES: &str = "/v2/users/{openid}/files";
const GROUP_FILES: &str = "/v2/groups/{openid}/files";
const USER_UPLOAD_PREPARE: &str = "/v2/users/{openid}/upload_prepare";
const GROUP_UPLOAD_PREPARE: &str = "/v2/groups/{openid}/upload_prepare";
const USER_UPLOAD_PART_FINISH: &str = "/v2/users/{openid}/upload_part_finish";
const GROUP_UPLOAD_PART_FINISH: &str = "/v2/groups/{openid}/upload_part_finish";
const MD5_10M_BYTES: usize = 10_002_432;
const USER_RECALL: &str = "/v2/users/{openid}/messages/{message_id}";
const GROUP_RECALL: &str = "/v2/groups/{openid}/messages/{message_id}";
const GROUP_INFO: &str = "/v2/groups/{group_openid}/info";
const GROUP_MEMBER: &str = "/v2/groups/{group_openid}/members/{member_openid}";
const GROUP_MEMBERS: &str = "/v2/groups/{group_openid}/members";
const GROUP_MUTE: &str = "/v2/groups/{group_openid}/restrict_chat_setting";
const GROUP_REMOVE_MEMBERS: &str = "/v2/groups/{group_openid}/batch_remove_members";
const GROUP_APPROVE_JOIN: &str = "/v2/groups/{group_openid}/approval_join_request/{member_openid}";
const GROUP_BOT_STATE: &str = "/v2/groups/{group_openid}/bot_state";
const GROUP_JOIN_REQUESTS: &str = "/v2/groups/{group_openid}/join_request_list";
const GROUP_BLACKLIST: &str = "/v2/groups/{group_openid}/member_blacklist";
const JOIN_STRATEGY: &str = "/v2/groups/join_approval_strategy";
const JOIN_STRATEGY_ITEM: &str = "/v2/groups/join_approval_strategy/{strategy_id}";
const JOIN_STRATEGY_EXECUTE: &str = "/v2/groups/join_approval_strategy/{strategy_id}/execute";
const JOIN_STRATEGY_WHITELIST: &str =
    "/v2/groups/join_approval_strategy/{strategy_id}/whitelist_users";
const GENERATE_URL_LINK: &str = "/v2/generate_url_link";
const MEDIA_IMAGE_PREPARE: &str = "/v2/media-images/prepare";
const MEDIA_IMAGE_COMPLETE: &str = "/v2/media-images/complete";
const CUSTOM_MENU: &str = "/v2/menu";
const PANELS: &str = "/v2/panels";
const PANEL: &str = "/v2/panels/{panel_id}";
const PANEL_TARGET: &str = "/v2/panels/{panel_id}/target";
const INTERACTION_ACK: &str = "/interactions/{interaction_id}";
const GATEWAY: &str = "/gateway";

/// Gateway 订阅的 intents 位掩码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Intents(u64);

impl Intents {
    /// 群聊与单聊消息事件（`GROUP_AND_C2C_EVENT`，1<<25）。
    pub const GROUP_AND_C2C: Self = Self(1 << 25);
    /// 群成员与入群申请事件（`GROUP_MEMBER_EVENT`，1<<24）。
    pub const GROUP_MEMBER: Self = Self(1 << 24);
    /// 按钮与菜单互动事件（`INTERACTION`，1<<26）。
    pub const INTERACTION: Self = Self(1 << 26);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Intents {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        self.union(other)
    }
}

impl Default for Intents {
    fn default() -> Self {
        Self::GROUP_AND_C2C
            .union(Self::INTERACTION)
            .union(Self::GROUP_MEMBER)
    }
}

/// QQ 官方 API 客户端。
pub struct Client {
    http: HttpClient,
    base: Url,
    token_url: Url,
    app_id: String,
    secret: String,
    token: Mutex<Option<CachedToken>>,
}

struct CachedToken {
    value: String,
    deadline: Instant,
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(deserialize_with = "deserialize_seconds")]
    expires_in: u64,
}

fn deserialize_seconds<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    let value = Value::deserialize(deserializer)?;
    match value {
        Value::Number(number) => number
            .as_u64()
            .ok_or_else(|| serde::de::Error::custom("invalid expires_in")),
        Value::String(text) => text
            .parse()
            .map_err(|_| serde::de::Error::custom("invalid expires_in")),
        _ => Err(serde::de::Error::custom("invalid expires_in")),
    }
}

#[derive(Deserialize)]
struct GatewayUrl {
    url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StreamRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_mode: Option<String>,
    pub input_state: u8,
    pub index: u32,
    pub content_type: String,
    pub content_raw: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_msg_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub msg_seq: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct MediaImagePrepare {
    upload_id: String,
    presigned_url: String,
}

#[derive(Debug, Deserialize)]
struct MediaImageComplete {
    url: String,
}

#[derive(Debug, Deserialize)]
struct ShareLink {
    data: ShareLinkData,
}

#[derive(Debug, Deserialize)]
struct ShareLinkData {
    url: String,
}

#[derive(Debug, Deserialize)]
struct UploadPrepare {
    upload_id: String,
    #[serde(default)]
    block_size: Option<Value>,
    #[serde(default)]
    parts: Vec<UploadPart>,
    #[serde(default)]
    upload_config: Option<UploadConfig>,
}

impl UploadPrepare {
    fn concurrency(&self) -> usize {
        self.upload_config
            .as_ref()
            .and_then(|config| config.concurrency.as_ref())
            .and_then(size_value)
            .unwrap_or(1)
            .clamp(1, 16)
    }

    fn retry_delay(&self) -> Duration {
        self.upload_config
            .as_ref()
            .and_then(|config| config.retry_delay.as_ref())
            .and_then(seconds_value)
            .map(Duration::from_secs_f64)
            .unwrap_or(Duration::from_secs(1))
            .min(Duration::from_secs(30))
    }
}

#[derive(Debug, Deserialize)]
struct UploadConfig {
    #[serde(default)]
    concurrency: Option<Value>,
    #[serde(default)]
    retry_delay: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
struct UploadPart {
    index: u64,
    presigned_url: String,
    #[serde(default)]
    block_size: Option<Value>,
}

fn size_value(value: &Value) -> Option<usize> {
    match value {
        Value::String(text) => text.parse().ok(),
        Value::Number(number) => number
            .as_u64()
            .and_then(|value| usize::try_from(value).ok()),
        _ => None,
    }
}

fn seconds_value(value: &Value) -> Option<f64> {
    match value {
        Value::String(text) => text.parse().ok(),
        Value::Number(number) => number.as_f64(),
        _ => None,
    }
}

fn retryable(error: &Error) -> bool {
    match error {
        Error::Network(_) => true,
        Error::Api {
            http_status, code, ..
        } => *http_status == 429 || *http_status >= 500 || *code == Some(40093001),
        _ => false,
    }
}

async fn file_hashes(path: &Path) -> Result<(u64, String, String, String), Error> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| Error::decode("cannot open local media file"))?;
    let mut md5 = Md5::new();
    let mut sha1 = Sha1::new();
    let mut md5_10m = Md5::new();
    let mut total = 0usize;
    let mut buffer = vec![0u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .await
            .map_err(|_| Error::decode("cannot read local media file"))?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        md5.update(chunk);
        sha1.update(chunk);
        if total < MD5_10M_BYTES {
            let take = read.min(MD5_10M_BYTES - total);
            md5_10m.update(&chunk[..take]);
        }
        total = total.saturating_add(read);
    }
    Ok((
        total as u64,
        hex(md5.finalize().as_slice()),
        hex(sha1.finalize().as_slice()),
        hex(md5_10m.finalize().as_slice()),
    ))
}

async fn read_part(path: &Path, offset: u64, size: usize) -> Result<Vec<u8>, Error> {
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|_| Error::decode("cannot open local media file"))?;
    file.seek(SeekFrom::Start(offset))
        .await
        .map_err(|_| Error::decode("cannot seek local media file"))?;
    let mut chunk = vec![0u8; size];
    file.read_exact(&mut chunk)
        .await
        .map_err(|_| Error::decode("cannot read local media part"))?;
    Ok(chunk)
}

fn md5_hex(data: &[u8]) -> String {
    hex(Md5::digest(data).as_slice())
}

fn sha1_hex(data: &[u8]) -> String {
    hex(Sha1::digest(data).as_slice())
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

impl Client {
    /// 使用官方地址构造客户端。
    pub fn new(app_id: impl Into<String>, secret: impl Into<String>) -> Result<Self, Error> {
        Self::with_endpoints(app_id, secret, API_BASE, TOKEN_URL)
    }

    /// 使用指定地址构造客户端。用于本地测试或私有部署。
    pub fn with_endpoints(
        app_id: impl Into<String>,
        secret: impl Into<String>,
        api_base: &str,
        token_url: &str,
    ) -> Result<Self, Error> {
        let app_id = app_id.into();
        let secret = secret.into();
        if app_id.is_empty() || secret.is_empty() {
            return Err(Error::auth("app_id and secret are required"));
        }
        if HeaderValue::from_str(&app_id).is_err() {
            return Err(Error::auth("app_id contains invalid header characters"));
        }
        let base = endpoint(api_base)?;
        let token_url = endpoint(token_url)?;
        let http = HttpClient::builder()
            .timeout(REQUEST_TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::network("cannot initialize HTTP client"))?;
        Ok(Self {
            http,
            base,
            token_url,
            app_id,
            secret,
            token: Mutex::new(None),
        })
    }

    /// 发送消息。`to` 决定单聊或群聊。
    pub async fn send_message(
        &self,
        to: &Peer,
        body: &OutgoingMessage,
    ) -> Result<SentMessage, Error> {
        let route = match to {
            Peer::User(_) => USER_MESSAGES,
            Peer::Group(_) => GROUP_MESSAGES,
        };
        let body = serde_json::to_value(body)
            .map_err(|_| Error::decode("cannot encode outgoing message"))?;
        self.request(Method::POST, route, &[to.openid()], &[], Some(&body))
            .await
    }

    /// 发送 C2C 流式消息分片。
    pub async fn send_stream(
        &self,
        user: &UserOpenId,
        body: &StreamRequest,
    ) -> Result<SentMessage, Error> {
        let body = serde_json::to_value(body)
            .map_err(|_| Error::decode("cannot encode stream message"))?;
        self.request(
            Method::POST,
            USER_STREAM_MESSAGES,
            &[user.as_str()],
            &[],
            Some(&body),
        )
        .await
    }

    /// 上传富媒体，返回可复用的 `file_info`。
    pub async fn upload_media(
        &self,
        to: &Peer,
        body: &MediaUpload,
    ) -> Result<UploadedMedia, Error> {
        let route = match to {
            Peer::User(_) => USER_FILES,
            Peer::Group(_) => GROUP_FILES,
        };
        let body =
            serde_json::to_value(body).map_err(|_| Error::decode("cannot encode media upload"))?;
        self.request(Method::POST, route, &[to.openid()], &[], Some(&body))
            .await
    }

    /// 分片上传本地媒体。`data` 是完整文件内容，适合超过 5M 直传上限的文件。
    ///
    /// 流程：`upload_prepare` → 逐片 PUT 到预签名 URL → `upload_part_finish` →
    /// 调用 `/files` 携带 `upload_id` 合并。文件只应传给同场景的接收方。
    pub async fn upload_media_chunked(
        &self,
        to: &Peer,
        file_type: u8,
        file_name: &str,
        data: &[u8],
    ) -> Result<UploadedMedia, Error> {
        if data.is_empty() {
            return Err(Error::decode("cannot upload empty media"));
        }
        let prepare_route = match to {
            Peer::User(_) => USER_UPLOAD_PREPARE,
            Peer::Group(_) => GROUP_UPLOAD_PREPARE,
        };
        let prepare_body = json!({
            "file_type": file_type,
            "file_size": data.len().to_string(),
            "file_name": file_name,
            "md5": md5_hex(data),
            "sha1": sha1_hex(data),
            "md5_10m": md5_hex(&data[..data.len().min(MD5_10M_BYTES)]),
        });
        let prepare: UploadPrepare = self
            .request(
                Method::POST,
                prepare_route,
                &[to.openid()],
                &[],
                Some(&prepare_body),
            )
            .await?;
        if prepare.upload_id.is_empty() {
            return Err(Error::decode("upload_prepare omitted upload_id"));
        }
        if prepare.parts.is_empty() {
            return Err(Error::decode("upload_prepare omitted parts"));
        }
        let default_size = prepare.block_size.as_ref().and_then(size_value);
        let mut offset = 0usize;
        for part in &prepare.parts {
            if part.presigned_url.is_empty() {
                return Err(Error::decode("upload part omitted presigned_url"));
            }
            let size = part
                .block_size
                .as_ref()
                .and_then(size_value)
                .or(default_size)
                .ok_or_else(|| Error::decode("upload part omitted block_size"))?;
            let end = offset
                .checked_add(size)
                .filter(|end| *end <= data.len())
                .ok_or_else(|| Error::decode("upload part size exceeds file"))?;
            let chunk = &data[offset..end];
            self.put_chunk(&part.presigned_url, chunk).await?;
            let finish_route = match to {
                Peer::User(_) => USER_UPLOAD_PART_FINISH,
                Peer::Group(_) => GROUP_UPLOAD_PART_FINISH,
            };
            let finish_body = json!({
                "upload_id": prepare.upload_id,
                "part_index": part.index,
                "block_size": chunk.len().to_string(),
                "md5": md5_hex(chunk),
            });
            let _: serde::de::IgnoredAny = self
                .request(
                    Method::POST,
                    finish_route,
                    &[to.openid()],
                    &[],
                    Some(&finish_body),
                )
                .await?;
            offset = end;
        }
        if offset != data.len() {
            return Err(Error::decode("upload parts do not cover the file"));
        }
        let complete_route = match to {
            Peer::User(_) => USER_FILES,
            Peer::Group(_) => GROUP_FILES,
        };
        let complete_body = json!({
            "file_type": file_type,
            "file_name": file_name,
            "file_size": data.len().to_string(),
            "upload_id": prepare.upload_id,
        });
        self.request(
            Method::POST,
            complete_route,
            &[to.openid()],
            &[],
            Some(&complete_body),
        )
        .await
    }

    /// 从本地文件分片上传，流式计算哈希，不把整个文件读入内存。
    pub async fn upload_file_chunked(
        &self,
        to: &Peer,
        file_type: u8,
        file_name: &str,
        path: &Path,
    ) -> Result<UploadedMedia, Error> {
        let (size, md5, sha1, md5_10m) = file_hashes(path).await?;
        if size == 0 {
            return Err(Error::decode("cannot upload empty media"));
        }
        let prepare = self
            .prepare_chunked(to, file_type, file_name, size, &md5, &sha1, &md5_10m)
            .await?;
        if prepare.upload_id.is_empty() {
            return Err(Error::decode("upload_prepare omitted upload_id"));
        }
        if prepare.parts.is_empty() {
            return Err(Error::decode("upload_prepare omitted parts"));
        }
        let default_size = prepare.block_size.as_ref().and_then(size_value);
        let delay = prepare.retry_delay();
        let concurrency = prepare.concurrency();
        let mut parts = Vec::with_capacity(prepare.parts.len());
        let mut offset = 0u64;
        for part in &prepare.parts {
            let part_size = part
                .block_size
                .as_ref()
                .and_then(size_value)
                .or(default_size)
                .ok_or_else(|| Error::decode("upload part omitted block_size"))?;
            let end = offset
                .checked_add(part_size as u64)
                .filter(|end| *end <= size)
                .ok_or_else(|| Error::decode("upload part size exceeds file"))?;
            parts.push((part.clone(), offset, part_size));
            offset = end;
        }
        if offset != size {
            return Err(Error::decode("upload parts do not cover the file"));
        }
        let upload_id = prepare.upload_id.clone();
        let mut stream =
            futures_util::stream::iter(parts.into_iter().map(|(part, offset, size)| {
                let upload_id = upload_id.clone();
                async move {
                    let chunk = read_part(path, offset, size).await?;
                    self.put_chunk_retry(&part.presigned_url, &chunk, delay)
                        .await?;
                    self.finish_part(to, &upload_id, &part, &chunk, delay).await
                }
            }))
            .buffer_unordered(concurrency);
        while let Some(result) = stream.next().await {
            result?;
        }
        self.complete_chunked(to, file_type, file_name, size, &prepare.upload_id)
            .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn prepare_chunked(
        &self,
        to: &Peer,
        file_type: u8,
        file_name: &str,
        size: u64,
        md5: &str,
        sha1: &str,
        md5_10m: &str,
    ) -> Result<UploadPrepare, Error> {
        let route = match to {
            Peer::User(_) => USER_UPLOAD_PREPARE,
            Peer::Group(_) => GROUP_UPLOAD_PREPARE,
        };
        let body = json!({
            "file_type": file_type,
            "file_size": size.to_string(),
            "file_name": file_name,
            "md5": md5,
            "sha1": sha1,
            "md5_10m": md5_10m,
        });
        self.request(Method::POST, route, &[to.openid()], &[], Some(&body))
            .await
    }

    async fn finish_part(
        &self,
        to: &Peer,
        upload_id: &str,
        part: &UploadPart,
        chunk: &[u8],
        delay: Duration,
    ) -> Result<(), Error> {
        let route = match to {
            Peer::User(_) => USER_UPLOAD_PART_FINISH,
            Peer::Group(_) => GROUP_UPLOAD_PART_FINISH,
        };
        let body = json!({
            "upload_id": upload_id,
            "part_index": part.index,
            "block_size": chunk.len().to_string(),
            "md5": md5_hex(chunk),
        });
        let mut attempt = 0u32;
        loop {
            match self
                .request::<serde::de::IgnoredAny>(
                    Method::POST,
                    route,
                    &[to.openid()],
                    &[],
                    Some(&body),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) if attempt < 2 && retryable(&error) => {
                    tokio::time::sleep(delay * (1 << attempt)).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn complete_chunked(
        &self,
        to: &Peer,
        file_type: u8,
        file_name: &str,
        size: u64,
        upload_id: &str,
    ) -> Result<UploadedMedia, Error> {
        let route = match to {
            Peer::User(_) => USER_FILES,
            Peer::Group(_) => GROUP_FILES,
        };
        let body = json!({
            "file_type": file_type,
            "file_name": file_name,
            "file_size": size.to_string(),
            "upload_id": upload_id,
        });
        self.request(Method::POST, route, &[to.openid()], &[], Some(&body))
            .await
    }

    async fn put_chunk_retry(&self, url: &str, chunk: &[u8], delay: Duration) -> Result<(), Error> {
        let mut attempt = 0u32;
        loop {
            match self.put_chunk(url, chunk).await {
                Ok(()) => return Ok(()),
                Err(error) if attempt < 2 && retryable(&error) => {
                    tokio::time::sleep(delay * (1 << attempt)).await;
                    attempt += 1;
                }
                Err(error) => return Err(error),
            }
        }
    }

    async fn put_chunk(&self, url: &str, chunk: &[u8]) -> Result<(), Error> {
        let response = self
            .http
            .put(url)
            .header("Content-Length", chunk.len())
            .body(chunk.to_vec())
            .send()
            .await
            .map_err(|error| network_error(&error))?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Error::Api {
                http_status: response.status().as_u16(),
                code: None,
                message: "chunk upload failed".into(),
            })
        }
    }

    /// 撤回机器人自己发送的消息。
    pub async fn recall(&self, to: &Peer, message_id: &str) -> Result<(), Error> {
        let message_id = path_segment(message_id, "message_id")?;
        let route = match to {
            Peer::User(_) => USER_RECALL,
            Peer::Group(_) => GROUP_RECALL,
        };
        self.request::<serde::de::IgnoredAny>(
            Method::DELETE,
            route,
            &[to.openid(), message_id],
            &[],
            None,
        )
        .await
        .map(|_| ())
    }

    /// 撤回群消息，可选择隐藏撤回提示。
    pub async fn recall_group(
        &self,
        group: &GroupOpenId,
        message_id: &str,
        hide_tip: bool,
    ) -> Result<(), Error> {
        let message_id = path_segment(message_id, "message_id")?;
        let query = if hide_tip {
            vec![("hidetip", "true")]
        } else {
            Vec::new()
        };
        self.request::<serde::de::IgnoredAny>(
            Method::DELETE,
            GROUP_RECALL,
            &[group.as_str(), message_id],
            &query,
            None,
        )
        .await
        .map(|_| ())
    }

    /// 批量移除群成员，单次最多 20 个。
    pub async fn remove_group_members(
        &self,
        group: &GroupOpenId,
        members: &[MemberOpenId],
        add_to_blacklist: bool,
    ) -> Result<(), Error> {
        if members.is_empty() || members.len() > 20 {
            return Err(Error::decode(
                "remove_group_members requires 1 to 20 members",
            ));
        }
        let openids = members.iter().map(MemberOpenId::as_str).collect::<Vec<_>>();
        let body = json!({
            "member_openids": openids,
            "add_to_member_blacklist": add_to_blacklist,
        });
        self.request::<serde::de::IgnoredAny>(
            Method::POST,
            GROUP_REMOVE_MEMBERS,
            &[group.as_str()],
            &[],
            Some(&body),
        )
        .await
        .map(|_| ())
    }

    pub async fn bot_group_state(&self, group: &GroupOpenId) -> Result<Value, Error> {
        self.request(Method::GET, GROUP_BOT_STATE, &[group.as_str()], &[], None)
            .await
    }

    pub async fn join_requests(
        &self,
        group: &GroupOpenId,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value, Error> {
        let mut query = Vec::new();
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            query.push(("cursor", cursor.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let query = query
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect::<Vec<_>>();
        self.request(
            Method::GET,
            GROUP_JOIN_REQUESTS,
            &[group.as_str()],
            &query,
            None,
        )
        .await
    }

    pub async fn group_blacklist(
        &self,
        group: &GroupOpenId,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value, Error> {
        let mut query = Vec::new();
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            query.push(("cursor", cursor.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let query = query
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect::<Vec<_>>();
        self.request(
            Method::GET,
            GROUP_BLACKLIST,
            &[group.as_str()],
            &query,
            None,
        )
        .await
    }

    pub async fn update_group_blacklist(
        &self,
        group: &GroupOpenId,
        op: &str,
        members: &[MemberOpenId],
    ) -> Result<Value, Error> {
        if !matches!(op, "add" | "del") || members.is_empty() || members.len() > 20 {
            return Err(Error::decode(
                "blacklist op must be add/del with 1 to 20 members",
            ));
        }
        let openids = members.iter().map(MemberOpenId::as_str).collect::<Vec<_>>();
        let body = json!({"op": op, "member_openids": openids});
        self.request(
            Method::POST,
            GROUP_BLACKLIST,
            &[group.as_str()],
            &[],
            Some(&body),
        )
        .await
    }

    pub async fn join_strategy_create(&self, body: &Value) -> Result<Value, Error> {
        self.request(Method::POST, JOIN_STRATEGY, &[], &[], Some(body))
            .await
    }

    pub async fn join_strategy_list(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value, Error> {
        let mut query = Vec::new();
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            query.push(("cursor", cursor.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let query = query
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect::<Vec<_>>();
        self.request(Method::GET, JOIN_STRATEGY, &[], &query, None)
            .await
    }

    pub async fn join_strategy_update(&self, id: &str, body: &Value) -> Result<Value, Error> {
        self.request(Method::PATCH, JOIN_STRATEGY_ITEM, &[id], &[], Some(body))
            .await
    }

    pub async fn join_strategy_execute(&self, id: &str) -> Result<(), Error> {
        self.request::<serde::de::IgnoredAny>(
            Method::POST,
            JOIN_STRATEGY_EXECUTE,
            &[id],
            &[],
            Some(&json!({})),
        )
        .await
        .map(|_| ())
    }

    pub async fn join_strategy_whitelist(&self, id: &str, body: &Value) -> Result<Value, Error> {
        self.request(
            Method::POST,
            JOIN_STRATEGY_WHITELIST,
            &[id],
            &[],
            Some(body),
        )
        .await
    }

    pub async fn join_strategy_delete(&self, id: &str) -> Result<(), Error> {
        self.request::<serde::de::IgnoredAny>(Method::DELETE, JOIN_STRATEGY_ITEM, &[id], &[], None)
            .await
            .map(|_| ())
    }

    pub async fn generate_share_link(&self, callback_data: Option<&str>) -> Result<String, Error> {
        let body = json!({"callback_data": callback_data.unwrap_or_default()});
        let response: ShareLink = self
            .request(Method::POST, GENERATE_URL_LINK, &[], &[], Some(&body))
            .await?;
        Ok(response.data.url)
    }

    pub async fn custom_menu(&self) -> Result<Value, Error> {
        self.request(Method::GET, CUSTOM_MENU, &[], &[], None).await
    }

    pub async fn put_custom_menu(&self, body: &Value) -> Result<Value, Error> {
        self.request(Method::PUT, CUSTOM_MENU, &[], &[], Some(body))
            .await
    }

    pub async fn panels(
        &self,
        scope: &str,
        cursor: Option<&str>,
        limit: Option<u32>,
    ) -> Result<Value, Error> {
        if scope.is_empty() {
            return Err(Error::decode("panel scope is required"));
        }
        let mut query = vec![("scope", scope.to_owned())];
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            query.push(("cursor", cursor.to_owned()));
        }
        if let Some(limit) = limit {
            query.push(("limit", limit.to_string()));
        }
        let query = query
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect::<Vec<_>>();
        self.request(Method::GET, PANELS, &[], &query, None).await
    }

    pub async fn create_panel(&self, body: &Value) -> Result<Value, Error> {
        self.request(Method::POST, PANELS, &[], &[], Some(body))
            .await
    }

    pub async fn panel(&self, id: &str) -> Result<Value, Error> {
        self.request(Method::GET, PANEL, &[id], &[], None).await
    }

    pub async fn update_panel(&self, id: &str, body: &Value) -> Result<Value, Error> {
        self.request(Method::PUT, PANEL, &[id], &[], Some(body))
            .await
    }

    pub async fn delete_panel(&self, id: &str) -> Result<(), Error> {
        self.request::<serde::de::IgnoredAny>(Method::DELETE, PANEL, &[id], &[], None)
            .await
            .map(|_| ())
    }

    pub async fn set_panel_target(&self, id: &str, body: &Value) -> Result<(), Error> {
        self.request::<serde::de::IgnoredAny>(Method::PUT, PANEL_TARGET, &[id], &[], Some(body))
            .await
            .map(|_| ())
    }

    /// 上传 Markdown 图片素材，返回 `qqbot-media://image/v1/<token>`。
    pub async fn upload_markdown_image(
        &self,
        data: &[u8],
        file_name: &str,
        content_type: &str,
    ) -> Result<String, Error> {
        if data.is_empty() || data.len() > 10_000_000 {
            return Err(Error::decode("markdown image must be 1 byte to 10 MB"));
        }
        let prepared: MediaImagePrepare = self
            .request(
                Method::POST,
                MEDIA_IMAGE_PREPARE,
                &[],
                &[],
                Some(&json!({
                    "file_name": file_name,
                    "file_size": data.len(),
                    "content_type": content_type,
                    "md5": md5_hex(data),
                    "sha1": sha1_hex(data),
                })),
            )
            .await?;
        if prepared.upload_id.is_empty() || prepared.presigned_url.is_empty() {
            return Err(Error::decode("media-image prepare omitted upload fields"));
        }
        let response = self
            .http
            .put(&prepared.presigned_url)
            .header("Content-Type", content_type)
            .body(data.to_vec())
            .send()
            .await
            .map_err(|error| network_error(&error))?;
        if !response.status().is_success() {
            return Err(Error::Api {
                http_status: response.status().as_u16(),
                code: None,
                message: "markdown image upload failed".into(),
            });
        }
        let etag = response
            .headers()
            .get("etag")
            .and_then(|value| value.to_str().ok())
            .map(|value| value.trim_matches('"').to_owned())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| md5_hex(data));
        let completed: MediaImageComplete = self
            .request(
                Method::POST,
                MEDIA_IMAGE_COMPLETE,
                &[],
                &[],
                Some(&json!({"upload_id": prepared.upload_id, "md5": etag})),
            )
            .await?;
        if completed.url.is_empty() {
            return Err(Error::decode("media-image complete omitted url"));
        }
        Ok(completed.url)
    }

    /// QQ 官方头像直链（openid 不是 QQ 号，必须使用 qqapp 形式）。
    pub fn avatar_url(&self, openid: &str) -> String {
        format!("https://q.qlogo.cn/qqapp/{}/{openid}/640", self.app_id)
    }

    /// 审批入群申请。
    pub async fn approve_join_request(
        &self,
        group: &GroupOpenId,
        member: &MemberOpenId,
        join_request_id: &str,
        approve: bool,
        reason: Option<&str>,
    ) -> Result<(), Error> {
        if join_request_id.is_empty() {
            return Err(Error::decode("join_request_id is empty"));
        }
        let mut body = json!({
            "op": if approve { "approve" } else { "decline" },
            "join_request_id": join_request_id,
        });
        if !approve && let Some(reason) = reason.filter(|reason| !reason.is_empty()) {
            body["reject_reason"] = Value::String(reason.to_owned());
        }
        self.request::<serde::de::IgnoredAny>(
            Method::POST,
            GROUP_APPROVE_JOIN,
            &[group.as_str(), member.as_str()],
            &[],
            Some(&body),
        )
        .await
        .map(|_| ())
    }

    pub async fn group_info(&self, group: &GroupOpenId) -> Result<GroupInfo, Error> {
        self.request(Method::GET, GROUP_INFO, &[group.as_str()], &[], None)
            .await
    }

    pub async fn group_member(
        &self,
        group: &GroupOpenId,
        member: &MemberOpenId,
    ) -> Result<Member, Error> {
        self.request(
            Method::GET,
            GROUP_MEMBER,
            &[group.as_str(), member.as_str()],
            &[],
            None,
        )
        .await
    }

    /// 查询一页群成员。`cursor` 为空表示第一页。
    pub async fn group_members(
        &self,
        group: &GroupOpenId,
        cursor: Option<&str>,
    ) -> Result<MemberPage, Error> {
        let mut query = Vec::new();
        if let Some(cursor) = cursor.filter(|cursor| !cursor.is_empty()) {
            query.push(("cursor", cursor));
        }
        self.request(Method::GET, GROUP_MEMBERS, &[group.as_str()], &query, None)
            .await
    }

    /// 批量设置群成员禁言状态。
    pub async fn set_member_mute(&self, group: &GroupOpenId, ops: &[MuteOp]) -> Result<(), Error> {
        if ops.is_empty() {
            return Err(Error::decode("at least one mute operation is required"));
        }
        let members = serde_json::to_value(ops)
            .map_err(|_| Error::decode("cannot encode mute operations"))?;
        let body = json!({ "members": members });
        self.request::<serde::de::IgnoredAny>(
            Method::POST,
            GROUP_MUTE,
            &[group.as_str()],
            &[],
            Some(&body),
        )
        .await
        .map(|_| ())
    }

    /// 回应互动事件。
    pub async fn ack_interaction(&self, id: &str, code: u8) -> Result<(), Error> {
        let id = path_segment(id, "interaction_id")?;
        let body = json!({ "code": code });
        self.request::<serde::de::IgnoredAny>(Method::PUT, INTERACTION_ACK, &[id], &[], Some(&body))
            .await
            .map(|_| ())
    }

    /// 获取 Gateway WebSocket 地址。
    pub async fn gateway_url(&self) -> Result<String, Error> {
        let response: GatewayUrl = self.request(Method::GET, GATEWAY, &[], &[], None).await?;
        if response.url.is_empty() {
            return Err(Error::decode("gateway response omitted url"));
        }
        Ok(response.url)
    }

    /// 获取 access token，临近过期时自动刷新。
    pub async fn access_token(&self) -> Result<String, Error> {
        let mut cached = self.token.lock().await;
        if let Some(token) = cached.as_ref()
            && token.deadline > Instant::now()
        {
            return Ok(token.value.clone());
        }
        let (value, expires_in) = self.fetch_token().await?;
        let lifetime = Duration::from_secs(expires_in);
        let margin = (lifetime / 10).min(TOKEN_MARGIN);
        let deadline = Instant::now() + lifetime.saturating_sub(margin);
        *cached = Some(CachedToken {
            value: value.clone(),
            deadline,
        });
        Ok(value)
    }

    async fn fetch_token(&self) -> Result<(String, u64), Error> {
        let started = Instant::now();
        let response = self
            .http
            .post(self.token_url.clone())
            .json(&json!({
                "appId": self.app_id,
                "clientSecret": self.secret,
            }))
            .send()
            .await
            .map_err(|error| network_error(&error))?;
        let status = response.status();
        let bytes = read_limited(response).await?;
        if status.as_u16() == 429 || status.is_server_error() {
            return Err(Error::network(format!(
                "token endpoint returned HTTP {}",
                status.as_u16()
            )));
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(Error::auth("token request was rejected"));
        }
        let value = serde_json::from_slice::<Value>(&bytes)
            .map_err(|_| Error::decode("invalid token response"))?;
        if let Some(code) = business_code(&value).filter(|code| *code != 0) {
            return Err(Error::auth(format!("QQ code {code}")));
        }
        let response: TokenResponse =
            serde_json::from_value(value).map_err(|_| Error::decode("invalid token response"))?;
        if response.access_token.is_empty() {
            return Err(Error::decode("token response omitted access_token"));
        }
        if response.expires_in == 0 {
            return Err(Error::decode("token response omitted expires_in"));
        }
        HeaderValue::from_str(&format!("QQBot {}", response.access_token))
            .map_err(|_| internal_header_error())?;
        tracing::debug!(
            http_status = status.as_u16(),
            elapsed_ms = started.elapsed().as_millis() as u64,
            "QQ access token refreshed"
        );
        Ok((response.access_token, response.expires_in))
    }

    async fn invalidate_token(&self) {
        *self.token.lock().await = None;
    }

    /// 所有 HTTP 接口的唯一入口。
    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        route: &'static str,
        path_args: &[&str],
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<T, Error> {
        let span = tracing::info_span!(
            "qq_api",
            method = method.as_str(),
            route = route,
            http_status = tracing::field::Empty
        );
        let started = Instant::now();
        async {
            if let Some(body) = body {
                tracing::debug!(body = %redact::value(body), "QQ API request body");
            }
            let result = self
                .execute_request::<T>(method, route, path_args, query, body)
                .await;
            match &result {
                Ok(_) => tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    "QQ API request completed"
                ),
                Err(error) => tracing::debug!(
                    elapsed_ms = started.elapsed().as_millis() as u64,
                    reason = %redact::text(&error.to_string()),
                    "QQ API request failed"
                ),
            }
            result
        }
        .instrument(span)
        .await
    }

    async fn execute_request<T: DeserializeOwned>(
        &self,
        method: Method,
        route: &str,
        path_args: &[&str],
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<T, Error> {
        let path = fill_route(route, path_args)?;
        let url = self
            .base
            .join(&path)
            .map_err(|_| Error::decode("cannot build API URL"))?;
        let token = self.access_token().await?;
        let mut request = self
            .http
            .request(method, url)
            .header("Authorization", format!("QQBot {token}"))
            .header("X-Union-Appid", &self.app_id);
        if let Some(body) = body {
            request = request.json(body);
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        let response = request
            .send()
            .await
            .map_err(|error| network_error(&error))?;
        let status = response.status();
        tracing::Span::current().record("http_status", status.as_u16());
        let bytes = read_limited(response).await?;
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            self.invalidate_token().await;
            return Err(Error::auth(format!("HTTP {}", status.as_u16())));
        }
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice::<Value>(&bytes).map_err(|_| {
                if status.is_success() {
                    Error::decode("response was not JSON")
                } else {
                    Error::Api {
                        http_status: status.as_u16(),
                        code: None,
                        message: "response was not JSON".into(),
                    }
                }
            })?
        };
        let body_code = business_code(&value);
        if let Some(code) = body_code
            && code != 0
        {
            if token_invalid(code) {
                self.invalidate_token().await;
            }
            return Err(Error::Api {
                http_status: status.as_u16(),
                code: Some(code),
                message: platform_message(&value),
            });
        }
        if !status.is_success() {
            return Err(Error::Api {
                http_status: status.as_u16(),
                code: body_code,
                message: platform_message(&value),
            });
        }
        if bytes.is_empty() {
            tracing::debug!(body = %redact::value(&value), "QQ API response body");
            return serde_json::from_value(value)
                .map_err(|_| Error::decode("empty response cannot be decoded"));
        }
        tracing::debug!(body = %redact::value(&value), "QQ API response body");
        serde_json::from_value(value).map_err(|_| Error::decode("response did not match schema"))
    }
}

fn endpoint(raw: &str) -> Result<Url, Error> {
    let url = Url::parse(raw).map_err(|_| Error::auth("invalid endpoint"))?;
    let loopback = match url.host() {
        Some(url::Host::Domain(host)) => host.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(host)) => host.is_loopback(),
        Some(url::Host::Ipv6(host)) => host.is_loopback(),
        None => false,
    };
    let scheme_ok = url.scheme() == "https" || (url.scheme() == "http" && loopback);
    if !scheme_ok
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(Error::auth(
            "endpoint must use HTTPS (or HTTP on loopback) without credentials, query or fragment",
        ));
    }
    Ok(url)
}

fn fill_route(route: &str, path_args: &[&str]) -> Result<String, Error> {
    let mut path = String::with_capacity(route.len() + 32);
    let mut rest = route;
    let mut index = 0;
    while let Some(start) = rest.find('{') {
        path.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            return Err(Error::decode("malformed route template"));
        };
        let name = &after[..end];
        if name.is_empty() {
            return Err(Error::decode("malformed route template"));
        }
        let Some(argument) = path_args.get(index) else {
            return Err(Error::decode("route argument missing"));
        };
        path.push_str(&encode_path_segment(argument));
        index += 1;
        rest = &after[end + 1..];
    }
    path.push_str(rest);
    if index != path_args.len() {
        return Err(Error::decode("unused route argument"));
    }
    Ok(path)
}

fn path_segment<'a>(value: &'a str, label: &str) -> Result<&'a str, Error> {
    if value.is_empty() {
        return Err(Error::decode(format!("invalid {label}")));
    }
    Ok(value)
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}

async fn read_limited(response: Response) -> Result<Vec<u8>, Error> {
    if response
        .content_length()
        .is_some_and(|length| length > BODY_LIMIT as u64)
    {
        return Err(Error::decode("response too large"));
    }
    let mut body = Vec::new();
    let mut response = response;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| network_error(&error))?
    {
        if chunk.len() > BODY_LIMIT - body.len() {
            return Err(Error::decode("response too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn network_error(error: &reqwest::Error) -> Error {
    if error.is_timeout() {
        Error::network("request timed out")
    } else if error.is_connect() {
        Error::network("connection failed")
    } else {
        Error::network("request failed")
    }
}

fn internal_header_error() -> Error {
    Error::auth("access token contains invalid header characters")
}

fn business_code(value: &Value) -> Option<i64> {
    for key in ["code", "err_code"] {
        if let Some(raw) = value.get(key) {
            return raw
                .as_i64()
                .or_else(|| raw.as_str().and_then(|text| text.parse().ok()));
        }
    }
    None
}

fn platform_message(value: &Value) -> String {
    for key in ["message", "msg", "err_msg"] {
        if let Some(message) = value.get(key).and_then(Value::as_str) {
            return message.to_owned();
        }
    }
    String::new()
}

fn token_invalid(code: i64) -> bool {
    matches!(
        code,
        40014 | 42001 | 11241 | 11242 | 11243 | 11244 | 11245 | 100007 | 100016
    )
}

impl From<InvalidHeaderValue> for Error {
    fn from(_: InvalidHeaderValue) -> Self {
        internal_header_error()
    }
}
