//! 合并转发：把 OB11 node 消息渲染为一条 QQ 自定义 Markdown。

use crate::{
    bridge::{Bridge, plan::ForwardOptions},
    error::{ActionError, NotFoundKind, UnsupportedKind},
    store::Store,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::future::BoxFuture;
use qbridge_ob11::{Media, Message, Node, Segment};
use serde_json::Value;
use std::{collections::HashMap, net::IpAddr, time::Duration};

/// 渲染节点集合。`options` 来自发送参数中的 `source`/`prompt`/`news`/`summary`。
pub async fn render(
    store: &Store,
    nodes: &[Node],
    options: &ForwardOptions,
    now: i64,
) -> Result<String, ActionError> {
    if nodes.is_empty() {
        return Err(ActionError::BadParam(
            "forward messages must contain at least one node".into(),
        ));
    }
    let mut renderer = Renderer {
        store,
        now,
        output: String::new(),
        image_sizes: HashMap::new(),
    };
    let title = options
        .source
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or("合并转发");
    renderer.push(&format!("# {}\n", escape(&single_line(title))));
    if let Some(prompt) = options
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.is_empty())
    {
        renderer.push(&format!("{}\n", escape(prompt)));
    }
    for news in &options.news {
        renderer.push(&format!("{}\n", escape(news)));
    }
    for node in nodes {
        renderer.render_node(node, 0).await?;
    }
    if let Some(summary) = options
        .summary
        .as_deref()
        .filter(|summary| !summary.is_empty())
    {
        renderer.push(&format!("\n\n{}", escape(summary)));
    }
    Ok(renderer.output)
}

/// 上传节点中的本地/Base64 图片，并把它们替换成 qqbot-media 引用。
pub async fn prepare_media(
    bridge: &Bridge,
    mut nodes: Vec<Node>,
) -> Result<Vec<Node>, ActionError> {
    for node in &mut nodes {
        prepare_node(bridge, node).await?;
    }
    Ok(nodes)
}

async fn prepare_node(bridge: &Bridge, node: &mut Node) -> Result<(), ActionError> {
    match node {
        Node::Custom { content, .. } => prepare_message(bridge, content).await,
        Node::Reference(_) => Ok(()),
    }
}

async fn prepare_message(bridge: &Bridge, message: &mut Message) -> Result<(), ActionError> {
    for segment in &mut message.0 {
        match segment {
            Segment::Image(media) => prepare_image(bridge, media).await?,
            Segment::Node(node) => Box::pin(prepare_node(bridge, node)).await?,
            _ => {}
        }
    }
    Ok(())
}

async fn prepare_image(bridge: &Bridge, media: &mut Media) -> Result<(), ActionError> {
    let file = media.file.clone();
    if file.starts_with("http://")
        || file.starts_with("https://")
        || file.starts_with("qqbot-media://")
    {
        return Ok(());
    }
    let (data, file_name) = if let Some(encoded) = file.strip_prefix("base64://") {
        let data = STANDARD
            .decode(encoded)
            .map_err(|_| ActionError::BadParam("invalid forward image base64".into()))?;
        (data, "forward.png".to_owned())
    } else {
        let path = if let Some(rest) = file.strip_prefix("file://") {
            std::path::PathBuf::from(percent_decode(rest))
        } else {
            std::path::PathBuf::from(&file)
        };
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("forward.png")
            .to_owned();
        let data = tokio::fs::read(&path)
            .await
            .map_err(|_| ActionError::BadParam("cannot read forward image".into()))?;
        (data, file_name)
    };
    let content_type = content_type(&file_name);
    let dimensions = imagesize::blob_size(&data).ok().map(|size| {
        (
            u32::try_from(size.width).unwrap_or(0),
            u32::try_from(size.height).unwrap_or(0),
        )
    });
    let url = bridge
        .qq
        .upload_markdown_image(&data, &file_name, content_type)
        .await?;
    media.file = url;
    media.url = None;
    if let Some((width, height)) = dimensions
        && width > 0
        && height > 0
    {
        media.extra.insert("width".into(), width.into());
        media.extra.insert("height".into(), height.into());
    }
    Ok(())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            output.push(byte);
            index += 3;
            continue;
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn content_type(file_name: &str) -> &'static str {
    match file_name
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

struct Renderer<'a> {
    store: &'a Store,
    now: i64,
    output: String,
    image_sizes: HashMap<String, Option<(u32, u32)>>,
}

impl Renderer<'_> {
    fn push(&mut self, text: &str) {
        self.output.push_str(text);
    }

    fn lines(&mut self, text: &str, depth: usize) {
        for part in text.split_inclusive('\n') {
            if self.output.ends_with('\n') {
                self.push(&"> ".repeat(depth));
            }
            self.push(part);
        }
    }

    fn render_node<'a>(
        &'a mut self,
        node: &'a Node,
        depth: usize,
    ) -> BoxFuture<'a, Result<(), ActionError>> {
        Box::pin(async move {
            let (title, content) = match node {
                Node::Reference(id) => {
                    let event = self
                        .store
                        .message(*id, self.now)
                        .await?
                        .ok_or(ActionError::NotFound(NotFoundKind::MessageNotCached))?;
                    let sender = event.get("sender").cloned().unwrap_or(Value::Null);
                    let message = event.get("message").cloned().ok_or_else(|| {
                        ActionError::BadParam("cached forward node has no message".into())
                    })?;
                    (cached_author(&sender), message)
                }
                Node::Custom {
                    user_id,
                    nickname,
                    content,
                    ..
                } => (
                    author_title(nickname.as_deref(), user_id.as_deref()),
                    content.to_value(),
                ),
            };
            let content = Message::from_value(content, false)
                .map_err(|error| ActionError::BadParam(error.to_string()))?;
            if content.0.is_empty() {
                return Err(ActionError::BadParam(
                    "forward node content must not be empty".into(),
                ));
            }
            if !self.output.ends_with('\n') {
                self.push("\n");
            }
            self.push("\n");
            self.lines(&format!("**{title}**\n"), depth + 1);
            let before = self.output.len();
            self.render_message(content, depth + 1).await?;
            if self.output.len() == before {
                return Err(ActionError::BadParam(
                    "forward node content must not be empty".into(),
                ));
            }
            Ok(())
        })
    }

    fn render_message<'a>(
        &'a mut self,
        message: Message,
        depth: usize,
    ) -> BoxFuture<'a, Result<(), ActionError>> {
        Box::pin(async move {
            for segment in message.0 {
                match segment {
                    Segment::Node(node) => self.render_node(&node, depth).await?,
                    Segment::Text(text) => self.lines(&escape(&text), depth),
                    Segment::Image(media) => {
                        let url = forward_image_url(&media)?;
                        let url = url
                            .replace('\\', "%5C")
                            .replace('(', "%28")
                            .replace(')', "%29");
                        if !self.output.ends_with('\n') {
                            self.push("\n");
                        }
                        let dimensions = if let (Some(width), Some(height)) = (
                            media.extra.get("width").and_then(Value::as_u64),
                            media.extra.get("height").and_then(Value::as_u64),
                        ) {
                            Some((width as u32, height as u32))
                        } else if let Some(dimensions) = self.image_sizes.get(&url) {
                            *dimensions
                        } else {
                            let dimensions = image_dimensions(&url).await;
                            self.image_sizes.insert(url.clone(), dimensions);
                            dimensions
                        };
                        self.lines(&format!("{}\n", image_markdown(&url, dimensions)), depth);
                    }
                    Segment::Other { .. }
                    | Segment::At(_)
                    | Segment::Reply(_)
                    | Segment::Face(_)
                    | Segment::Record(_)
                    | Segment::Video(_)
                    | Segment::File(_) => {
                        return Err(ActionError::Unsupported(UnsupportedKind::Segment));
                    }
                }
            }
            Ok(())
        })
    }
}

fn image_markdown(url: &str, dimensions: Option<(u32, u32)>) -> String {
    match dimensions {
        Some((width, height)) => format!("![图片 #{width}px #{height}px]({url})"),
        None => format!("![图片]({url})"),
    }
}

async fn image_dimensions(url: &str) -> Option<(u32, u32)> {
    let parsed = url::Url::parse(url).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    // 不探测本机/内网地址，避免 forward 图片尺寸请求变成 SSRF。
    if let Some(host) = parsed.host_str() {
        if host.eq_ignore_ascii_case("localhost") || host.ends_with(".local") {
            return None;
        }
        if let Ok(ip) = host.parse::<IpAddr>() {
            let blocked = match ip {
                IpAddr::V4(ip) => {
                    ip.is_loopback() || ip.is_private() || ip.is_link_local() || ip.is_unspecified()
                }
                IpAddr::V6(ip) => ip.is_loopback() || ip.is_unspecified(),
            };
            if blocked {
                return None;
            }
        }
    }
    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(800))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .ok()?;
    let response = client.get(url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    if response
        .content_length()
        .is_some_and(|length| length > 15 * 1024 * 1024)
    {
        return None;
    }
    let bytes = response.bytes().await.ok()?;
    if bytes.len() > 15 * 1024 * 1024 {
        return None;
    }
    let size = imagesize::blob_size(&bytes).ok()?;
    let width = u32::try_from(size.width).ok()?;
    let height = u32::try_from(size.height).ok()?;
    (width > 0 && height > 0).then_some((width, height))
}

fn forward_image_url(media: &qbridge_ob11::Media) -> Result<String, ActionError> {
    let url = media.url.as_deref().unwrap_or(&media.file);
    if url.starts_with("https://")
        || url.starts_with("http://")
        || url.starts_with("qqbot-media://")
    {
        return Ok(url.to_owned());
    }
    Err(ActionError::Unsupported(UnsupportedKind::ForwardMedia))
}

fn cached_author(sender: &Value) -> String {
    let name = sender
        .get("nickname")
        .and_then(Value::as_str)
        .or_else(|| sender.get("card").and_then(Value::as_str));
    let id = sender.get("user_id").and_then(|value| {
        value
            .as_i64()
            .map(|id| id.to_string())
            .or_else(|| value.as_u64().map(|id| id.to_string()))
            .or_else(|| value.as_str().map(str::to_owned))
    });
    author_title(name, id.as_deref())
}

fn author_title(name: Option<&str>, id: Option<&str>) -> String {
    let name = name
        .map(single_line)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "消息".into());
    let title = match id.filter(|id| !id.is_empty()) {
        Some(id) => format!("{name} ({id})"),
        None => name,
    };
    escape(&title)
}

fn single_line(value: &str) -> String {
    value.replace(['\n', '\r', '\t'], " ")
}

fn escape(text: &str) -> String {
    let mut result = String::new();
    for character in text.chars() {
        match character {
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            '\r' => {}
            character if character.is_control() && character != '\n' && character != '\t' => {}
            character => {
                if "\\`*_{}[]()#+-.!|~".contains(character) {
                    result.push('\\');
                }
                result.push(character);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::image_markdown;

    #[test]
    fn markdown_image_uses_dimensions_only_when_known() {
        assert_eq!(
            image_markdown("https://example.com/a.png", Some((290, 75))),
            "![图片 #290px #75px](https://example.com/a.png)"
        );
        assert_eq!(
            image_markdown("https://example.com/a.png", None),
            "![图片](https://example.com/a.png)"
        );
    }
}
