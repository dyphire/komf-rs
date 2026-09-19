//! 通知上下文 —— 对应 `NotificationContext.kt`。
use std::collections::HashMap;

/// 通知上下文：在一次元数据更新完成后发送给通知渠道。
#[derive(Debug, Clone, Default)]
pub struct NotificationContext {
    pub library: LibraryContext,
    pub series: SeriesContext,
    pub books: Vec<BookContext>,
    pub media_server: String,
    pub series_cover: Option<Vec<u8>>,
    pub series_cover_mime_type: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LibraryContext {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Default)]
pub struct SeriesContext {
    pub id: String,
    pub name: String,
    pub book_count: i32,
    pub metadata: SeriesMetadataContext,
}

#[derive(Debug, Clone, Default)]
pub struct SeriesMetadataContext {
    pub status: String,
    pub title: String,
    pub title_sort: String,
    pub alternative_titles: Vec<AlternativeTitleContext>,
    pub summary: String,
    pub reading_direction: Option<String>,
    pub publisher: Option<String>,
    pub alternative_publishers: Vec<String>,
    pub age_rating: Option<i32>,
    pub language: Option<String>,
    pub genres: Vec<String>,
    pub tags: Vec<String>,
    pub total_book_count: Option<i32>,
    pub authors: Vec<AuthorContext>,
    pub release_year: Option<i32>,
    pub links: Vec<WebLinkContext>,
}

#[derive(Debug, Clone, Default)]
pub struct BookContext {
    pub id: String,
    pub name: String,
    pub number: i32,
    pub metadata: BookMetadataContext,
}

#[derive(Debug, Clone, Default)]
pub struct BookMetadataContext {
    pub title: String,
    pub summary: Option<String>,
    pub number: String,
    pub number_sort: Option<String>,
    pub release_date: Option<String>,
    pub authors: Vec<AuthorContext>,
    pub tags: Vec<String>,
    pub isbn: Option<String>,
    pub links: Vec<WebLinkContext>,
}

#[derive(Debug, Clone, Default)]
pub struct AlternativeTitleContext {
    pub label: String,
    pub title: String,
}

#[derive(Debug, Clone, Default)]
pub struct AuthorContext {
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Default)]
pub struct WebLinkContext {
    pub label: String,
    pub url: String,
}

/// 将上下文转换为模板引擎的值树。
pub fn to_value_tree(context: &NotificationContext) -> crate::velocity::Value {
    use crate::velocity::Value;
    let mut library = HashMap::new();
    library.insert("id".to_string(), Value::Str(context.library.id.clone()));
    library.insert("name".to_string(), Value::Str(context.library.name.clone()));

    let mut metadata = HashMap::new();
    let m = &context.series.metadata;
    metadata.insert("status".to_string(), Value::Str(m.status.clone()));
    metadata.insert("title".to_string(), Value::Str(m.title.clone()));
    metadata.insert("titleSort".to_string(), Value::Str(m.title_sort.clone()));
    metadata.insert(
        "alternativeTitles".to_string(),
        Value::List(
            m.alternative_titles
                .iter()
                .map(|t| {
                    let mut map = HashMap::new();
                    map.insert("label".to_string(), Value::Str(t.label.clone()));
                    map.insert("title".to_string(), Value::Str(t.title.clone()));
                    Value::Map(map)
                })
                .collect(),
        ),
    );
    metadata.insert("summary".to_string(), Value::Str(m.summary.clone()));
    metadata.insert(
        "readingDirection".to_string(),
        opt_str(m.reading_direction.clone()),
    );
    metadata.insert("publisher".to_string(), opt_str(m.publisher.clone()));
    metadata.insert(
        "alternativePublishers".to_string(),
        Value::List(m.alternative_publishers.iter().map(|p| Value::Str(p.clone())).collect()),
    );
    metadata.insert("ageRating".to_string(), opt_int(m.age_rating));
    metadata.insert("language".to_string(), opt_str(m.language.clone()));
    metadata.insert(
        "genres".to_string(),
        Value::List(m.genres.iter().map(|g| Value::Str(g.clone())).collect()),
    );
    metadata.insert(
        "tags".to_string(),
        Value::List(m.tags.iter().map(|t| Value::Str(t.clone())).collect()),
    );
    metadata.insert("totalBookCount".to_string(), opt_int(m.total_book_count));
    metadata.insert(
        "authors".to_string(),
        Value::List(
            m.authors
                .iter()
                .map(|a| {
                    let mut map = HashMap::new();
                    map.insert("name".to_string(), Value::Str(a.name.clone()));
                    map.insert("role".to_string(), Value::Str(a.role.clone()));
                    Value::Map(map)
                })
                .collect(),
        ),
    );
    metadata.insert("releaseYear".to_string(), opt_int(m.release_year));
    metadata.insert(
        "links".to_string(),
        Value::List(
            m.links
                .iter()
                .map(|l| {
                    let mut map = HashMap::new();
                    map.insert("label".to_string(), Value::Str(l.label.clone()));
                    map.insert("url".to_string(), Value::Str(l.url.clone()));
                    Value::Map(map)
                })
                .collect(),
        ),
    );

    let mut series = HashMap::new();
    series.insert("id".to_string(), Value::Str(context.series.id.clone()));
    series.insert("name".to_string(), Value::Str(context.series.name.clone()));
    series.insert("bookCount".to_string(), Value::Int(context.series.book_count as i64));
    series.insert("metadata".to_string(), Value::Map(metadata));

    let mut books = Vec::new();
    let mut sorted: Vec<&BookContext> = context.books.iter().collect();
    sorted.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    for book in sorted {
        let mut bm = HashMap::new();
        bm.insert("title".to_string(), Value::Str(book.metadata.title.clone()));
        bm.insert("summary".to_string(), opt_str(book.metadata.summary.clone()));
        bm.insert("number".to_string(), Value::Str(book.metadata.number.clone()));
        bm.insert("numberSort".to_string(), opt_str(book.metadata.number_sort.clone()));
        bm.insert("releaseDate".to_string(), opt_str(book.metadata.release_date.clone()));
        bm.insert(
            "authors".to_string(),
            Value::List(
                book.metadata
                    .authors
                    .iter()
                    .map(|a| {
                        let mut map = HashMap::new();
                        map.insert("name".to_string(), Value::Str(a.name.clone()));
                        map.insert("role".to_string(), Value::Str(a.role.clone()));
                        Value::Map(map)
                    })
                    .collect(),
            ),
        );
        bm.insert(
            "tags".to_string(),
            Value::List(book.metadata.tags.iter().map(|t| Value::Str(t.clone())).collect()),
        );
        bm.insert("isbn".to_string(), opt_str(book.metadata.isbn.clone()));
        bm.insert(
            "links".to_string(),
            Value::List(
                book.metadata
                    .links
                    .iter()
                    .map(|l| {
                        let mut map = HashMap::new();
                        map.insert("label".to_string(), Value::Str(l.label.clone()));
                        map.insert("url".to_string(), Value::Str(l.url.clone()));
                        Value::Map(map)
                    })
                    .collect(),
            ),
        );

        let mut b = HashMap::new();
        b.insert("id".to_string(), Value::Str(book.id.clone()));
        b.insert("name".to_string(), Value::Str(book.name.clone()));
        b.insert("number".to_string(), Value::Int(book.number as i64));
        b.insert("metadata".to_string(), Value::Map(bm));
        books.push(Value::Map(b));
    }

    let mut root = HashMap::new();
    root.insert("library".to_string(), Value::Map(library));
    root.insert("series".to_string(), Value::Map(series));
    root.insert("books".to_string(), Value::List(books));
    root.insert("mediaServer".to_string(), Value::Str(context.media_server.clone()));
    Value::Map(root)
}

fn opt_str(value: Option<String>) -> crate::velocity::Value {
    match value {
        Some(v) => crate::velocity::Value::Str(v),
        None => crate::velocity::Value::Null,
    }
}

fn opt_int(value: Option<i32>) -> crate::velocity::Value {
    match value {
        Some(v) => crate::velocity::Value::Int(v as i64),
        None => crate::velocity::Value::Null,
    }
}
