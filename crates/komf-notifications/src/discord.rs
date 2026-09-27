//! Discord webhook 通知 —— 对应 `DiscordWebhookService.kt` 与模板渲染。
use crate::context::{to_value_tree, NotificationContext};
use crate::velocity::{Template, Value};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

const BASE_URL: &str = "https://discord.com/api";

#[derive(Debug, Clone)]
pub struct DiscordRenderResult {
    pub title: Option<String>,
    pub title_url: Option<String>,
    pub description: Option<String>,
    pub fields: Vec<EmbedField>,
    pub footer: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DiscordStringTemplates {
    pub title_template: Option<String>,
    pub title_url_template: Option<String>,
    pub description_template: Option<String>,
    pub field_templates: Vec<FieldStringTemplates>,
    pub footer_template: Option<String>,
}

#[derive(Debug, Clone)]
pub struct FieldStringTemplates {
    pub name_template: String,
    pub value_template: String,
    pub inline: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    pub inline: bool,
}

/// Discord 模板渲染器 —— 对应 `DiscordVelocityTemplates.kt`。
///
/// 从 `<templatesDirectory>/discord/` 目录加载 `title.vm`、`title_url.vm`、
/// `description.vm`、`footer.vm` 及 `field_<n>_name[_inline].vm` / `field_<n>_value.vm`；
/// 目录不存在时使用内置默认模板。
#[derive(Clone)]
pub struct DiscordVelocityTemplates {
    directory: String,
    state: Arc<RwLock<DiscordTemplateState>>,
}

#[derive(Clone)]
struct DiscordTemplateState {
    title: Option<Template>,
    title_url: Option<Template>,
    description: Option<Template>,
    footer: Option<Template>,
    fields: Vec<FieldTemplates>,
}

#[derive(Clone)]
struct FieldTemplates {
    name: Template,
    value: Template,
    inline: bool,
}

const TITLE_FILE: &str = "title.vm";
const TITLE_URL_FILE: &str = "title_url.vm";
const DESCRIPTION_FILE: &str = "description.vm";
const FOOTER_FILE: &str = "footer.vm";

pub fn default_title_template() -> String {
    "$series.name".to_string()
}

pub fn default_description_template() -> String {
    r#"#if (${series.metadata.summary} != "")
${series.metadata.summary}
#end
***new #if(${books.size()} == 1)book was #{else}books were #{end}added to library ${library.name}:***
#foreach ($book in $books)
**${book.name}**
#end
"#
    .to_string()
}

impl DiscordVelocityTemplates {
    pub fn new(template_directory: &str) -> Self {
        let directory = std::path::Path::new(template_directory)
            .join("discord")
            .to_string_lossy()
            .to_string();
        let state = DiscordTemplateState {
            title: load_file_template(&directory, TITLE_FILE),
            title_url: load_file_template(&directory, TITLE_URL_FILE),
            description: load_file_template(&directory, DESCRIPTION_FILE),
            footer: load_file_template(&directory, FOOTER_FILE),
            fields: load_field_templates(&directory),
        };
        Self {
            directory,
            state: Arc::new(RwLock::new(state)),
        }
    }

    pub fn render(&self, context: &NotificationContext) -> DiscordRenderResult {
        let state = self.state.read().unwrap();
        let root = to_value_tree(context);
        render_state(&state, &root)
    }

    pub fn render_with(&self, context: &NotificationContext, templates: &DiscordStringTemplates) -> DiscordRenderResult {
        let state = DiscordTemplateState {
            title: templates.title_template.as_deref().and_then(|t| Template::parse(t).ok()),
            title_url: templates
                .title_url_template
                .as_deref()
                .and_then(|t| Template::parse(t).ok()),
            description: templates
                .description_template
                .as_deref()
                .and_then(|t| Template::parse(t).ok()),
            footer: templates.footer_template.as_deref().and_then(|t| Template::parse(t).ok()),
            fields: templates
                .field_templates
                .iter()
                .filter_map(|f| {
                    Some(FieldTemplates {
                        name: Template::parse(&f.name_template).ok()?,
                        value: Template::parse(&f.value_template).ok()?,
                        inline: f.inline,
                    })
                })
                .collect(),
        };
        let root = to_value_tree(context);
        render_state(&state, &root)
    }

    pub fn get_current_templates(&self) -> DiscordStringTemplates {
        let directory = &self.directory;
        let title = read_file_or(directory, TITLE_FILE, &default_title_template());
        let title_url = read_file_opt(directory, TITLE_URL_FILE);
        let description = read_file_or(directory, DESCRIPTION_FILE, &default_description_template());
        let footer = read_file_opt(directory, FOOTER_FILE);
        let fields = read_field_string_templates(directory);
        DiscordStringTemplates {
            title_template: Some(title),
            title_url_template: title_url,
            description_template: Some(description),
            field_templates: fields,
            footer_template: footer,
        }
    }

    pub fn update_templates(&self, templates: &DiscordStringTemplates) {
        std::fs::create_dir_all(&self.directory).ok();
        let dir = std::path::Path::new(&self.directory);

        write_template_file(dir, TITLE_FILE, templates.title_template.as_deref());
        write_template_file(dir, TITLE_URL_FILE, templates.title_url_template.as_deref());
        write_template_file(dir, DESCRIPTION_FILE, templates.description_template.as_deref());
        write_template_file(dir, FOOTER_FILE, templates.footer_template.as_deref());

        // 清理旧的 field_* 文件
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("field_") && name.ends_with(".vm") {
                    std::fs::remove_file(entry.path()).ok();
                }
            }
        }
        for (index, field) in templates.field_templates.iter().enumerate() {
            let inline = if field.inline { "_inline" } else { "" };
            let name_file = dir.join(format!("field_{}_name{}.vm", index + 1, inline));
            let value_file = dir.join(format!("field_{}_value.vm", index + 1));
            std::fs::write(name_file, &field.name_template).ok();
            std::fs::write(value_file, &field.value_template).ok();
        }

        let state = DiscordTemplateState {
            title: load_file_template(&self.directory, TITLE_FILE),
            title_url: load_file_template(&self.directory, TITLE_URL_FILE),
            description: load_file_template(&self.directory, DESCRIPTION_FILE),
            footer: load_file_template(&self.directory, FOOTER_FILE),
            fields: load_field_templates(&self.directory),
        };
        *self.state.write().unwrap() = state;
    }
}

fn render_state(state: &DiscordTemplateState, root: &Value) -> DiscordRenderResult {
    let render = |tpl: &Option<Template>| tpl.as_ref().map(|t| t.render(root.clone()));
    let title = render(&state.title).map(|t| truncate(&t, 256));
    let title_url = render(&state.title_url).map(|t| t.trim().to_string());
    let description = render(&state.description).map(|t| truncate(&t, 4095));
    let footer = render(&state.footer).map(|t| truncate(&t, 2048));
    let fields = state
        .fields
        .iter()
        .map(|f| EmbedField {
            name: truncate(&f.name.render(root.clone()), 256),
            value: truncate(&f.value.render(root.clone()), 1024),
            inline: f.inline,
        })
        .collect();
    DiscordRenderResult {
        title,
        title_url,
        description,
        fields,
        footer,
    }
}

fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn load_file_template(directory: &str, file: &str) -> Option<Template> {
    let path = std::path::Path::new(directory).join(file);
    let content = std::fs::read_to_string(path).ok()?;
    Template::parse(&content).ok()
}

fn read_file_or(directory: &str, file: &str, default: &str) -> String {
    let path = std::path::Path::new(directory).join(file);
    std::fs::read_to_string(path).unwrap_or_else(|_| default.to_string())
}

fn read_file_opt(directory: &str, file: &str) -> Option<String> {
    let path = std::path::Path::new(directory).join(file);
    std::fs::read_to_string(path).ok()
}

fn write_template_file(dir: &std::path::Path, file: &str, content: Option<&str>) {
    let path = dir.join(file);
    match content {
        Some(content) if !content.trim().is_empty() => {
            std::fs::write(path, content).ok();
        }
        _ => {
            std::fs::remove_file(path).ok();
        }
    }
}

fn load_field_templates(directory: &str) -> Vec<FieldTemplates> {
    read_field_files(directory)
        .into_iter()
        .filter_map(|(name, value, inline)| {
            Some(FieldTemplates {
                name: Template::parse(&name).ok()?,
                value: Template::parse(&value).ok()?,
                inline,
            })
        })
        .collect()
}

fn read_field_string_templates(directory: &str) -> Vec<FieldStringTemplates> {
    read_field_files(directory)
        .into_iter()
        .map(|(name, value, inline)| FieldStringTemplates {
            name_template: name,
            value_template: value,
            inline,
        })
        .collect()
}

/// 读取 `field_<n>_name[_inline].vm` / `field_<n>_value.vm` 配对文件。
fn read_field_files(directory: &str) -> Vec<(String, String, bool)> {
    let dir = std::path::Path::new(directory);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut name_map: HashMap<u32, (String, bool)> = HashMap::new();
    let mut value_map: HashMap<u32, String> = HashMap::new();

    for entry in entries.flatten() {
        let file_name = entry.file_name().to_string_lossy().to_string();
        if !file_name.starts_with("field_") || !file_name.ends_with(".vm") {
            continue;
        }
        let stem = file_name.trim_end_matches(".vm");
        let parts: Vec<&str> = stem.split('_').collect();
        if parts.len() < 3 {
            continue;
        }
        let Ok(number) = parts[1].parse::<u32>() else {
            continue;
        };
        match parts[2] {
            "name" => {
                let inline = parts.len() > 3 && parts[3] == "inline";
                let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
                name_map.insert(number, (content, inline));
            }
            "value" => {
                let content = std::fs::read_to_string(entry.path()).unwrap_or_default();
                value_map.insert(number, content);
            }
            _ => {}
        }
    }

    let mut numbers: Vec<u32> = name_map.keys().copied().collect();
    numbers.sort();
    numbers
        .into_iter()
        .filter_map(|key| {
            let (name, inline) = name_map.get(&key)?;
            let value = value_map.get(&key)?;
            Some((name.clone(), value.clone(), *inline))
        })
        .collect()
}

/// Discord webhook 服务 —— 对应 `DiscordWebhookService.kt`。
#[derive(Clone)]
pub struct DiscordWebhookService {
    client: reqwest::Client,
    webhooks: Vec<String>,
    series_cover: bool,
    embed_color: u32,
    template_renderer: DiscordVelocityTemplates,
    /// 对齐 Kotlin discordKtorClient 的 HttpRequestRateLimiter（2s/4 事件，无突发）。
    limiter: Arc<ThroughputLimiter>,
}

#[derive(Debug, Serialize)]
struct WebhookExecuteRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    embeds: Option<Vec<Embed>>,
}

#[derive(Debug, Serialize)]
struct Embed {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    footer: Option<EmbedFooter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<EmbedImage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<Vec<EmbedField>>,
}

#[derive(Debug, Serialize)]
struct EmbedFooter {
    text: String,
}

#[derive(Debug, Serialize)]
struct EmbedImage {
    url: String,
}

#[derive(Debug, serde::Deserialize)]
struct Webhook {
    id: String,
    token: String,
}

/// 平滑节流限速器 —— 对应 Kotlin `rateLimiter(eventsPerInterval, interval)`
/// （NotificationsModule 中 discord client 配置：interval=2s、eventsPerInterval=4、allowBurst=false）。
/// 每个许可占用 `interval / events` 的槽位，超出时阻塞到下一个可用槽位。
struct ThroughputLimiter {
    permit_duration: Duration,
    cursor: Mutex<Instant>,
}

impl ThroughputLimiter {
    fn new(events_per_interval: u32, interval: Duration) -> Self {
        let permit_duration = interval.div_f32(events_per_interval as f32);
        Self {
            permit_duration,
            cursor: Mutex::new(Instant::now()),
        }
    }

    /// 获取一个许可；需要等待时阻塞（对齐 Kotlin RateLimiterImpl.acquire）。
    async fn acquire(&self) {
        let now = Instant::now();
        let sleep = {
            let mut cursor = self.cursor.lock().unwrap();
            let base = if *cursor > now { *cursor } else { now };
            *cursor = base + self.permit_duration;
            base.saturating_duration_since(now)
        };
        if !sleep.is_zero() {
            tokio::time::sleep(sleep).await;
        }
    }
}

impl DiscordWebhookService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: reqwest::Client,
        webhooks: Vec<String>,
        series_cover: bool,
        embed_color: String,
        template_renderer: DiscordVelocityTemplates,
    ) -> Self {
        let embed_color = u32::from_str_radix(embed_color.trim_start_matches('#'), 16).unwrap_or(0x1F8B4C);
        Self {
            client,
            webhooks,
            series_cover,
            embed_color,
            template_renderer,
            limiter: Arc::new(ThroughputLimiter::new(4, Duration::from_secs(2))),
        }
    }

    /// 发送请求并应用 Kotlin discordKtorClient 的重试与限速策略：
    /// - 每次请求前经限速器 acquire（2s 窗口 4 个许可，500ms 槽位）；
    /// - 429 / 5xx 重试最多 3 次，指数退避 1s/2s/4s（上限 10s），尊重 Retry-After 头。
    async fn send_with_retry<F>(&self, build: F) -> crate::Result<reqwest::Response>
    where
        F: Fn() -> reqwest::RequestBuilder,
    {
        let mut attempt: u32 = 0;
        loop {
            self.limiter.acquire().await;
            let response = build().send().await?;
            let status = response.status();
            if status.is_success() || attempt >= 3 {
                return Ok(response);
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error() {
                // 尊重 Retry-After 头；否则指数退避 1s * 2^attempt（上限 10s）。
                let retry_after = response
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);
                let backoff = Duration::from_secs(1u64 << attempt.min(3)).min(Duration::from_secs(10));
                let delay = retry_after.filter(|d| *d > backoff).unwrap_or(backoff);
                attempt += 1;
                tokio::time::sleep(delay).await;
            } else {
                return Ok(response);
            }
        }
    }

    pub async fn send(
        &self,
        context: &NotificationContext,
        templates: Option<&DiscordStringTemplates>,
    ) -> crate::Result<()> {
        if self.webhooks.is_empty() {
            return Ok(());
        }
        let render_result = match templates {
            Some(templates) => self.template_renderer.render_with(context, templates),
            None => self.template_renderer.render(context),
        };
        if render_result.description.is_none()
            && render_result.fields.is_empty()
            && render_result.footer.is_none()
            && render_result.title.is_none()
            && !self.series_cover
        {
            tracing::warn!("empty discord message for series {}. Skipping notification", context.series.name);
            return Ok(());
        }

        let image = if self.series_cover {
            context
                .series_cover
                .as_ref()
                .map(|_| {
                    let ext = context
                        .series_cover_mime_type
                        .as_deref()
                        .and_then(|m| m.strip_prefix("image/"))
                        .unwrap_or("jpeg");
                    EmbedImage {
                        url: format!("attachment://cover.{ext}"),
                    }
                })
        } else {
            None
        };

        let embed = Embed {
            title: render_result.title,
            url: render_result.title_url,
            description: render_result.description,
            color: Some(self.embed_color),
            footer: render_result.footer.map(|text| EmbedFooter { text }),
            image,
            fields: if render_result.fields.is_empty() {
                None
            } else {
                Some(render_result.fields)
            },
        };
        let request = WebhookExecuteRequest {
            content: None,
            embeds: Some(vec![embed]),
        };

        for webhook_url in &self.webhooks {
            // Kotlin `getWebhook` 用 expectSuccess=true 的 client，非 2xx 抛 ResponseException。
            let webhook: Webhook = self
                .send_with_retry(|| self.client.get(webhook_url))
                .await?
                .error_for_status()?
                .json()
                .await?;
            let url = format!("{BASE_URL}/webhooks/{}/{}", webhook.id, webhook.token);
            let payload = serde_json::to_string(&request)?;

            let response = if let Some(cover) = &context.series_cover {
                let ext = context
                    .series_cover_mime_type
                    .as_deref()
                    .and_then(|m| m.strip_prefix("image/"))
                    .unwrap_or("jpeg");
                let mime = context
                    .series_cover_mime_type
                    .clone()
                    .unwrap_or_else(|| "image/jpeg".to_string());
                // 预验证 mime（mime_str 失败视为错误，与 Kotlin multipart 行为一致）
                reqwest::multipart::Part::bytes(Vec::new())
                    .mime_str(&mime)
                    .map_err(crate::NotificationError::Http)?;
                self.send_with_retry(|| {
                    let form = reqwest::multipart::Form::new()
                        .part(
                            "cover",
                            reqwest::multipart::Part::bytes(cover.clone())
                                .file_name(format!("cover.{ext}"))
                                .mime_str(&mime)
                                .unwrap(),
                        )
                        .text("payload_json", payload.clone());
                    self.client.post(&url).multipart(form)
                })
                .await?
            } else {
                self.send_with_retry(|| self.client.post(&url).json(&request))
                    .await?
            };
            // Kotlin executeWebhook 非 2xx 抛 ResponseException（透传上游状态码）；
            // Rust 同样返回错误，由 HTTP 端点透传状态码。
            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(crate::NotificationError::Upstream { status, body });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rate_limiter_smooths_bursts() {
        // 对应 Kotlin rateLimiter(events, interval)：permit 间隔 = interval/events。
        // 平滑节流：第 1 个许可立即，之后每个间隔一个（不允许突发）。
        let limiter = ThroughputLimiter::new(4, Duration::from_millis(400)); // permit = 100ms
        assert_eq!(limiter.permit_duration, Duration::from_millis(100));

        let start = Instant::now();
        limiter.acquire().await;
        assert!(start.elapsed() < Duration::from_millis(50), "first permit should be immediate");

        for _ in 0..3 {
            limiter.acquire().await;
        }
        let after_four = start.elapsed();
        assert!(after_four >= Duration::from_millis(280), "four permits should span ~300ms, got {after_four:?}");

        limiter.acquire().await; // 第 5 个许可在 400ms 槽位
        let after_five = start.elapsed();
        assert!(after_five >= Duration::from_millis(380), "fifth permit should wait until 400ms, got {after_five:?}");
        assert!(after_five < Duration::from_millis(800), "got {after_five:?}");
    }
}
