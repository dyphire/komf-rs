//! 通知路由 —— 对应 `NotificationRoutes.kt`。
use crate::routes::SharedState;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use komf_api_models::common::KomfErrorResponse;
use komf_api_models::notifications::{
    KomfAppriseRenderResult, KomfAppriseRequest, KomfAppriseTemplates, KomfDiscordRenderField,
    KomfDiscordRenderResult, KomfDiscordRequest, KomfDiscordTemplateField, KomfDiscordTemplates,
    KomfNotificationContext,
};
use komf_notifications::apprise::AppriseStringTemplates;
use komf_notifications::context::{
    AlternativeTitleContext, AuthorContext, BookContext, BookMetadataContext, LibraryContext,
    NotificationContext, SeriesContext, SeriesMetadataContext, WebLinkContext,
};
use komf_notifications::discord::{
    DiscordStringTemplates, FieldStringTemplates,
};

pub fn router() -> Router<SharedState> {
    Router::new()
        .route(
            "/notifications/discord/templates",
            get(discord_get_templates).post(discord_update_templates),
        )
        .route("/notifications/discord/send", axum::routing::post(discord_send))
        .route("/notifications/discord/render", axum::routing::post(discord_render))
        .route(
            "/notifications/apprise/templates",
            get(apprise_get_templates).post(apprise_update_templates),
        )
        .route("/notifications/apprise/send", axum::routing::post(apprise_send))
        .route("/notifications/apprise/render", axum::routing::post(apprise_render))
}

async fn discord_get_templates(State(state): State<SharedState>) -> impl IntoResponse {
    let state = state.read().unwrap();
    let templates = state.discord_renderer.get_current_templates();
    Json(to_discord_templates_dto(&templates))
}

async fn discord_update_templates(
    State(state): State<SharedState>,
    Json(request): Json<KomfDiscordTemplates>,
) -> Result<impl IntoResponse, (StatusCode, Json<KomfErrorResponse>)> {
    let state = state.read().unwrap();
    let templates = from_discord_templates_dto(&request);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.discord_renderer.update_templates(&templates)
    })) {
        Ok(_) => Ok((StatusCode::OK, Json(request))),
        Err(panic) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(KomfErrorResponse {
                message: format!("TemplateParseException: {panic:?}"),
            }),
        )),
    }
}

async fn discord_send(
    State(state): State<SharedState>,
    Json(request): Json<KomfDiscordRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<KomfErrorResponse>)> {
    let discord_service = {
        let state = state.read().unwrap();
        state.discord_service.clone()
    };
    let context = context_to_model(&request.context);
    let templates = request.templates.as_ref().map(from_discord_templates_dto);
    match discord_service.send(&context, templates.as_ref()).await {
        Ok(_) => Ok((StatusCode::OK, Json(""))),
        // Kotlin：catch (ResponseException) → 透传上游状态码，
        // 消息格式 "${exception::class.simpleName} ${response.bodyAsText()}"
        // （Ktor 4xx 为 ClientRequestException、5xx 为 ServerResponseException）。
        Err(komf_notifications::NotificationError::Upstream { status, body }) => {
            let prefix = if status.is_client_error() {
                "ClientRequestException"
            } else {
                "ServerResponseException"
            };
            Err((
                status,
                Json(KomfErrorResponse {
                    message: format!("{prefix} {body}"),
                }),
            ))
        }
        Err(error) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(KomfErrorResponse { message: error.to_string() }),
        )),
    }
}

async fn discord_render(
    State(state): State<SharedState>,
    Json(request): Json<KomfDiscordRequest>,
) -> impl IntoResponse {
    let state = state.read().unwrap();
    let context = context_to_model(&request.context);
    let templates = request.templates.as_ref().map(from_discord_templates_dto);
    let result = match templates.as_ref() {
        Some(templates) => state.discord_renderer.render_with(&context, templates),
        None => state.discord_renderer.render(&context),
    };
    Json(KomfDiscordRenderResult {
        title: result.title,
        title_url: result.title_url,
        description: result.description,
        footer: result.footer,
        fields: result
            .fields
            .into_iter()
            .map(|f| KomfDiscordRenderField {
                name: f.name,
                value: f.value,
                inline: f.inline,
            })
            .collect(),
    })
}

async fn apprise_get_templates(State(state): State<SharedState>) -> impl IntoResponse {
    let state = state.read().unwrap();
    let templates = state.apprise_renderer.get_current_templates();
    Json(KomfAppriseTemplates {
        title: templates.title_template,
        body: templates.body_template,
    })
}

async fn apprise_update_templates(
    State(state): State<SharedState>,
    Json(request): Json<KomfAppriseTemplates>,
) -> Result<impl IntoResponse, (StatusCode, Json<KomfErrorResponse>)> {
    let state = state.read().unwrap();
    let templates = AppriseStringTemplates {
        title_template: request.title.clone(),
        body_template: request.body.clone(),
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        state.apprise_renderer.update_templates(&templates)
    })) {
        Ok(_) => Ok((StatusCode::OK, Json(request))),
        Err(panic) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(KomfErrorResponse {
                message: format!("TemplateParseException: {panic:?}"),
            }),
        )),
    }
}

async fn apprise_send(
    State(state): State<SharedState>,
    Json(request): Json<KomfAppriseRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<KomfErrorResponse>)> {
    let state = state.read().unwrap();
    let context = context_to_model(&request.context);
    let templates = request.templates.as_ref().map(|t| AppriseStringTemplates {
        title_template: t.title.clone(),
        body_template: t.body.clone(),
    });
    match state.apprise_service.send(&context, templates.as_ref()) {
        Ok(_) => Ok((StatusCode::OK, Json(""))),
        Err(error) => Err((
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(KomfErrorResponse { message: error.to_string() }),
        )),
    }
}

async fn apprise_render(
    State(state): State<SharedState>,
    Json(request): Json<KomfAppriseRequest>,
) -> impl IntoResponse {
    let state = state.read().unwrap();
    let context = context_to_model(&request.context);
    let templates = request.templates.as_ref().map(|t| AppriseStringTemplates {
        title_template: t.title.clone(),
        body_template: t.body.clone(),
    });
    let result = match templates.as_ref() {
        Some(templates) => state.apprise_renderer.render_with(&context, templates),
        None => state.apprise_renderer.render(&context),
    };
    Json(KomfAppriseRenderResult {
        title: result.title,
        body: result.body,
    })
}

// ---------------------------------------------------------------------------
// DTO 转换
// ---------------------------------------------------------------------------

fn context_to_model(context: &KomfNotificationContext) -> NotificationContext {
    NotificationContext {
        library: LibraryContext {
            id: context.library.id.clone(),
            name: context.library.name.clone(),
        },
        series: SeriesContext {
            id: context.series.id.clone(),
            name: context.series.name.clone(),
            book_count: context.series.book_count,
            metadata: SeriesMetadataContext {
                status: context.series.metadata.status.clone(),
                title: context.series.metadata.title.clone(),
                title_sort: context.series.metadata.title_sort.clone(),
                alternative_titles: context
                    .series
                    .metadata
                    .alternative_titles
                    .iter()
                    .map(|t| AlternativeTitleContext {
                        label: t.label.clone(),
                        title: t.title.clone(),
                    })
                    .collect(),
                summary: context.series.metadata.summary.clone(),
                reading_direction: context.series.metadata.reading_direction.clone(),
                publisher: context.series.metadata.publisher.clone(),
                alternative_publishers: context.series.metadata.alternative_publishers.clone(),
                age_rating: context.series.metadata.age_rating,
                language: context.series.metadata.language.clone(),
                genres: context.series.metadata.genres.clone(),
                tags: context.series.metadata.tags.clone(),
                total_book_count: context.series.metadata.total_book_count,
                authors: context
                    .series
                    .metadata
                    .authors
                    .iter()
                    .map(|a| AuthorContext {
                        name: a.name.clone(),
                        role: a.role.clone(),
                    })
                    .collect(),
                release_year: context.series.metadata.release_year,
                links: context
                    .series
                    .metadata
                    .links
                    .iter()
                    .map(|l| WebLinkContext {
                        label: l.label.clone(),
                        url: l.url.clone(),
                    })
                    .collect(),
            },
        },
        books: context
            .books
            .iter()
            .map(|book| BookContext {
                id: book.id.clone(),
                name: book.name.clone(),
                number: book.number,
                metadata: BookMetadataContext {
                    title: book.metadata.title.clone(),
                    summary: book.metadata.summary.clone(),
                    number: book.metadata.number.clone(),
                    number_sort: book.metadata.number_sort.clone(),
                    release_date: book.metadata.release_date.clone(),
                    authors: book
                        .metadata
                        .authors
                        .iter()
                        .map(|a| AuthorContext {
                            name: a.name.clone(),
                            role: a.role.clone(),
                        })
                        .collect(),
                    tags: book.metadata.tags.clone(),
                    isbn: book.metadata.isbn.clone(),
                    links: book
                        .metadata
                        .links
                        .iter()
                        .map(|l| WebLinkContext {
                            label: l.label.clone(),
                            url: l.url.clone(),
                        })
                        .collect(),
                },
            })
            .collect(),
        media_server: context.media_server.clone(),
        series_cover: None,
        series_cover_mime_type: None,
    }
}

fn to_discord_templates_dto(templates: &DiscordStringTemplates) -> KomfDiscordTemplates {
    KomfDiscordTemplates {
        title: templates.title_template.clone(),
        title_url: templates.title_url_template.clone(),
        description: templates.description_template.clone(),
        footer: templates.footer_template.clone(),
        fields: templates
            .field_templates
            .iter()
            .map(|f| KomfDiscordTemplateField {
                name: Some(f.name_template.clone()),
                value: Some(f.value_template.clone()),
                inline: Some(f.inline),
            })
            .collect(),
    }
}

fn from_discord_templates_dto(templates: &KomfDiscordTemplates) -> DiscordStringTemplates {
    DiscordStringTemplates {
        title_template: templates.title.clone(),
        title_url_template: templates.title_url.clone(),
        description_template: templates.description.clone(),
        field_templates: templates
            .fields
            .iter()
            .map(|f| FieldStringTemplates {
                name_template: f.name.clone().unwrap_or_default(),
                value_template: f.value.clone().unwrap_or_default(),
                inline: f.inline.unwrap_or(false),
            })
            .collect(),
        footer_template: templates.footer.clone(),
    }
}
