//! Apprise CLI 通知 —— 对应 `AppriseCliService.kt` 与模板渲染。
use crate::context::{to_value_tree, NotificationContext};
use crate::velocity::{Template, Value};
use std::process::Command;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Default)]
pub struct AppriseRenderResult {
    pub title: Option<String>,
    pub body: String,
}

#[derive(Debug, Clone, Default)]
pub struct AppriseStringTemplates {
    pub title_template: Option<String>,
    pub body_template: Option<String>,
}

/// Apprise 模板渲染器 —— 对应 `AppriseVelocityTemplates.kt`。
///
/// 从 `<templatesDirectory>/apprise/` 目录加载 `apprise_title.vm`、
/// `apprise_body.vm`；目录不存在时使用内置默认模板。
#[derive(Clone)]
pub struct AppriseVelocityTemplates {
    directory: String,
    state: Arc<RwLock<AppriseTemplateState>>,
}

#[derive(Clone)]
struct AppriseTemplateState {
    title: Option<Template>,
    body: Option<Template>,
}

const TITLE_FILE: &str = "apprise_title.vm";
const BODY_FILE: &str = "apprise_body.vm";

pub fn default_title_template() -> String {
    "$series.name".to_string()
}

pub fn default_body_template() -> String {
    r#"#if (${series.metadata.summary} != "")
${series.metadata.summary}
#end
new #if(${books.size()} == 1)book was #{else}books were #{end}added to library ${library.name}:
#foreach ($book in $books)
${book.name}
#end
"#
    .to_string()
}

impl AppriseVelocityTemplates {
    pub fn new(template_directory: &str) -> Self {
        let directory = std::path::Path::new(template_directory)
            .join("apprise")
            .to_string_lossy()
            .to_string();
        let state = AppriseTemplateState {
            title: load_template(&directory, TITLE_FILE),
            body: load_template(&directory, BODY_FILE),
        };
        Self {
            directory,
            state: Arc::new(RwLock::new(state)),
        }
    }

    pub fn render(&self, context: &NotificationContext) -> AppriseRenderResult {
        let state = self.state.read().unwrap();
        render_state(&state, context)
    }

    pub fn render_with(&self, context: &NotificationContext, templates: &AppriseStringTemplates) -> AppriseRenderResult {
        let state = AppriseTemplateState {
            title: templates.title_template.as_deref().and_then(|t| Template::parse(t).ok()),
            body: templates.body_template.as_deref().and_then(|t| Template::parse(t).ok()),
        };
        render_state(&state, context)
    }

    pub fn get_current_templates(&self) -> AppriseStringTemplates {
        AppriseStringTemplates {
            title_template: Some(read_file_or(&self.directory, TITLE_FILE, &default_title_template())),
            body_template: Some(read_file_or(&self.directory, BODY_FILE, &default_body_template())),
        }
    }

    pub fn update_templates(&self, templates: &AppriseStringTemplates) {
        std::fs::create_dir_all(&self.directory).ok();
        let dir = std::path::Path::new(&self.directory);
        write_file(dir, TITLE_FILE, templates.title_template.as_deref());
        write_file(dir, BODY_FILE, templates.body_template.as_deref());
        let state = AppriseTemplateState {
            title: load_template(&self.directory, TITLE_FILE),
            body: load_template(&self.directory, BODY_FILE),
        };
        *self.state.write().unwrap() = state;
    }
}

fn render_state(state: &AppriseTemplateState, context: &NotificationContext) -> AppriseRenderResult {
    let root: Value = to_value_tree(context);
    let title = state.title.as_ref().map(|t| t.render(root.clone()));
    let body = state
        .body
        .as_ref()
        .map(|t| t.render(root.clone()))
        .unwrap_or_default();
    AppriseRenderResult { title, body }
}

fn load_template(directory: &str, file: &str) -> Option<Template> {
    let path = std::path::Path::new(directory).join(file);
    let content = std::fs::read_to_string(path).ok()?;
    Template::parse(&content).ok()
}

fn read_file_or(directory: &str, file: &str, default: &str) -> String {
    let path = std::path::Path::new(directory).join(file);
    std::fs::read_to_string(path).unwrap_or_else(|_| default.to_string())
}

fn write_file(dir: &std::path::Path, file: &str, content: Option<&str>) {
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

/// Apprise CLI 服务 —— 对应 `AppriseCliService.kt`。
///
/// 将渲染后的消息通过 `apprise` 命令行工具发送到各 url。
#[derive(Clone)]
pub struct AppriseCliService {
    urls: Vec<String>,
    template_renderer: AppriseVelocityTemplates,
    series_cover: bool,
}

impl AppriseCliService {
    pub fn new(urls: Vec<String>, template_renderer: AppriseVelocityTemplates, series_cover: bool) -> Self {
        Self {
            urls,
            template_renderer,
            series_cover,
        }
    }

    pub fn send(
        &self,
        context: &NotificationContext,
        templates: Option<&AppriseStringTemplates>,
    ) -> anyhow::Result<()> {
        if self.urls.is_empty() {
            return Ok(());
        }

        let cover_path = self.get_cover_attachment(context)?;
        let result = (|| {
            let render_result = match templates {
                Some(templates) => self.template_renderer.render_with(context, templates),
                None => self.template_renderer.render(context),
            };

            let mut args: Vec<String> = Vec::new();
            if let Some(title) = &render_result.title {
                args.push("-t".to_string());
                args.push(title.clone());
            }
            args.push("-b".to_string());
            args.push(render_result.body);
            if let Some(path) = &cover_path {
                args.push("--attach".to_string());
                args.push(path.to_string_lossy().to_string());
            }
            args.extend(self.urls.iter().cloned());

            let output = Command::new("apprise").args(&args).output()?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                tracing::error!("apprise error: {stderr}");
                anyhow::bail!("Apprise returned non zero exit code: {stderr}");
            }
            Ok(())
        })();
        if let Some(path) = cover_path {
            std::fs::remove_file(path).ok();
        }
        result
    }

    fn get_cover_attachment(&self, context: &NotificationContext) -> anyhow::Result<Option<std::path::PathBuf>> {
        if !self.series_cover {
            return Ok(None);
        }
        let Some(cover) = &context.series_cover else {
            return Ok(None);
        };
        let extension = context
            .series_cover_mime_type
            .as_deref()
            .and_then(mime_guess::get_mime_extensions_str)
            .and_then(|exts| exts.first().copied())
            .unwrap_or("jpg");
        let tmp_dir = std::env::temp_dir();
        let file_name = format!("{}_{}.{}", context.series.name.replace(|c: char| !c.is_alphanumeric(), "_"), std::process::id(), extension);
        let path = tmp_dir.join(file_name);
        std::fs::write(&path, cover)?;
        Ok(Some(path))
    }
}
