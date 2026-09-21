//! mylar 格式 series.json 导出器 —— 参考 komga-mylar.py 的 `export_series_as_mylar_json`。
//!
//! 与 ComicInfo 并列的第二种元数据导出：在 `UpdateMode::MylarSeriesJson` 启用时，
//! 把 post-processing 后的系列元数据写成 `<系列目录>/series.json`
//! （oneshot 系列为 `<系列目录>/<系列名>.oneshot.json`），字段结构对齐 py 脚本
//! （version 1.0.2；`comicid`/`year` 为 mylar 占位，year 被 releaseDate 年份覆盖）。
//! 配置 `mylarCovers` 开启时同时下载系列封面为 `cover.jpg`（oneshot 为 `<系列名>.cover.jpg`）。

use komf_core::model::{AuthorRole, ReadingDirection, ReleaseDate, SeriesMetadata, SeriesStatus, TitleType};
use serde::Serialize;
use std::path::Path;

/// py 脚本固定版本号。
pub const MYLAR_VERSION: &str = "1.0.2";
/// py 脚本占位 comicid（mylar 导入时按名称匹配替换）。
pub const MYLAR_COMICID_PLACEHOLDER: i32 = 9527;
/// py 脚本占位 year（releaseDate 有年份时覆盖）。
pub const MYLAR_YEAR_PLACEHOLDER: i32 = 2001;

#[derive(Debug, Clone, Serialize)]
pub struct MylarSeriesJson {
    pub version: String,
    pub metadata: MylarMetadata,
}

/// mylar metadata 字段（字段名与 py 脚本完全一致：snake_case 与 camelCase 混用照抄）。
#[derive(Debug, Clone, Serialize)]
pub struct MylarMetadata {
    #[serde(rename = "type")]
    pub r#type: String,
    pub publisher: String,
    pub imprint: Option<()>,
    pub name: String,
    pub comicid: i32,
    pub year: i32,
    pub description_text: String,
    pub description_formatted: Option<()>,
    pub volume: Option<()>,
    pub booktype: String,
    pub age_rating: Option<String>,
    pub collects: Option<()>,
    pub comic_image: String,
    pub total_issues: i32,
    pub publication_run: String,
    pub status: String,
    pub language: Option<String>,
    #[serde(rename = "readingDirection")]
    pub reading_direction: Option<String>,
    #[serde(rename = "releaseDate")]
    pub release_date: Option<String>,
    pub authors: Option<Vec<MylarAuthor>>,
    pub links: Option<Vec<MylarLink>>,
    #[serde(rename = "alternateTitles")]
    pub alternate_titles: Option<Vec<MylarAlternateTitle>>,
    pub genres: Option<Vec<String>>,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MylarAuthor {
    pub name: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MylarLink {
    pub label: String,
    pub url: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct MylarAlternateTitle {
    pub label: String,
    pub title: String,
}

/// 由 post-processing 后的系列元数据构造 mylar series.json。
/// `books_count` 为系列当前书籍数（totalBookCount 缺失时的回退，对齐 py `totalBookCount or booksCount`）。
pub fn mylar_series_json_from_metadata(series: &SeriesMetadata, books_count: i32) -> MylarSeriesJson {
    // 对齐 py：name = metadata.title or series.name；Rust 侧 title 为处理后主标题。
    let name = series.title_name().unwrap_or_default();
    // 对齐 py：year 初始占位 2001，releaseDate 前 4 位为数字时覆盖。
    let year = series
        .release_date
        .as_ref()
        .and_then(|d| d.year)
        .unwrap_or(MYLAR_YEAR_PLACEHOLDER);
    let alternate_titles = {
        // 对齐 Kotlin `titles.filter { it != title }`（SeriesTitle 全等剔除主标题）
        // + komga patch 的按 title 去重（保留首个）。
        let mut seen = std::collections::HashSet::new();
        let titles = series
            .titles
            .iter()
            .filter(|t| match &series.title {
                Some(pt) => *t != pt,
                None => true,
            })
            .filter_map(|t| {
                let label = title_label(t.r#type, t.language.as_deref())?;
                seen.insert(t.name.clone()).then_some(MylarAlternateTitle {
                    label,
                    title: t.name.clone(),
                })
            })
            .collect::<Vec<_>>();
        if titles.is_empty() {
            None
        } else {
            Some(titles)
        }
    };
    let authors = if series.authors.is_empty() {
        None
    } else {
        Some(
            series
                .authors
                .iter()
                .map(|a| MylarAuthor {
                    name: a.name.clone(),
                    role: role_to_string(&a.role),
                })
                .collect(),
        )
    };
    let links = if series.links.is_empty() {
        None
    } else {
        Some(
            series
                .links
                .iter()
                .map(|l| MylarLink {
                    label: l.label.clone(),
                    url: l.url.clone(),
                })
                .collect(),
        )
    };
    let genres = if series.genres.is_empty() {
        None
    } else {
        Some(series.genres.clone())
    };
    let tags = if series.tags.is_empty() {
        None
    } else {
        Some(series.tags.clone())
    };

    MylarSeriesJson {
        version: MYLAR_VERSION.to_string(),
        metadata: MylarMetadata {
            r#type: "comicSeries".to_string(),
            publisher: series
                .publisher
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or_default(),
            imprint: None,
            name,
            comicid: MYLAR_COMICID_PLACEHOLDER,
            year,
            description_text: series.summary.clone().unwrap_or_default(),
            description_formatted: None,
            volume: None,
            booktype: "Print".to_string(),
            age_rating: normalize_age_rating(series.age_rating),
            collects: None,
            comic_image: String::new(),
            total_issues: series.total_book_count.unwrap_or(books_count).max(0),
            publication_run: String::new(),
            status: mylar_status(series.status),
            language: series.language.clone(),
            reading_direction: series.reading_direction.map(reading_direction_to_string),
            release_date: series.release_date.as_ref().map(release_date_to_string),
            authors,
            links,
            alternate_titles,
            genres,
            tags,
        },
    }
}

/// 对齐 py `normalize_age_rating`：<=0 → All、<12 → 9+、<15 → 12+、<17 → 15+、<18 → 17+、>=18 → Adult。
pub fn normalize_age_rating(value: Option<i32>) -> Option<String> {
    let v = value?;
    Some(
        match v {
            v if v <= 0 => "All",
            v if v < 12 => "9+",
            v if v < 15 => "12+",
            v if v < 17 => "15+",
            v if v < 18 => "17+",
            _ => "Adult",
        }
        .to_string(),
    )
}

/// 对齐 py 状态映射：ONGOING/HIATUS/ABANDONED → Continuing、ENDED → Ended；None → Continuing。
/// Rust 额外 Completed（Kotlin 对齐，komga patch 中 Completed|Ended → ENDED）：→ Ended。
pub fn mylar_status(status: Option<SeriesStatus>) -> String {
    match status {
        Some(SeriesStatus::Ended | SeriesStatus::Completed) => "Ended".to_string(),
        _ => "Continuing".to_string(),
    }
}

fn reading_direction_to_string(d: ReadingDirection) -> String {
    match d {
        ReadingDirection::LeftToRight => "LEFT_TO_RIGHT".to_string(),
        ReadingDirection::RightToLeft => "RIGHT_TO_LEFT".to_string(),
        ReadingDirection::Vertical => "VERTICAL".to_string(),
        ReadingDirection::Webtoon => "WEBTOON".to_string(),
    }
}

/// ReleaseDate → "YYYY-MM-DD"（月/日缺失补 01，对齐 Komga releaseDate 字符串形态）。
fn release_date_to_string(d: &ReleaseDate) -> String {
    let year = d.year.unwrap_or_default();
    let month = d.month.unwrap_or(1);
    let day = d.day.unwrap_or(1);
    format!("{year:04}-{month:02}-{day:02}")
}

/// 对齐 komga `to_series_update_request` 的 alternateTitles label：
/// ROMAJI/NATIVE → 类型标签；LOCALIZED → 语言（缺省回退类型标签）；null 类型 → 语言；
/// 无语言可作 label → None（过滤丢弃）。
/// 注：komga patch 处 label 可被 `alternateTitleLabels` 配置覆盖；mylar 导出用默认标签。
fn title_label(title_type: Option<TitleType>, language: Option<&str>) -> Option<String> {
    match title_type {
        Some(TitleType::Romaji) => Some(TitleType::Romaji.label().to_string()),
        Some(TitleType::Native) => Some(TitleType::Native.label().to_string()),
        Some(TitleType::Localized) => Some(
            language
                .unwrap_or_else(|| TitleType::Localized.label())
                .to_string(),
        ),
        None => language.map(|l| l.to_string()),
    }
}

/// 对齐 py：透传 Komga booksMetadata.authors（Komga 标准 AuthorRole 序列化为小写
/// writer/penciller/inker/colorist/letterer/cover/editor/translator），而非 komf 内部大写枚举名。
fn role_to_string(role: &AuthorRole) -> String {
    match role {
        AuthorRole::Writer => "writer",
        AuthorRole::Penciller => "penciller",
        AuthorRole::Inker => "inker",
        AuthorRole::Colorist => "colorist",
        AuthorRole::Letterer => "letterer",
        AuthorRole::Cover => "cover",
        AuthorRole::Editor => "editor",
        AuthorRole::Translator => "translator",
    }
    .to_string()
}

/// 解析 mylar 导出目录与 oneshot 文件名基准（对齐 py --output / --library-root，修正 oneshot 路径）：
/// - `url`：系列 url（普通系列=目录；oneshot=zip 文件路径）
/// - `output_dir`：Some(根目录) → 导出到 根目录/相对结构 或 根目录/目录名；None → 系列原目录
/// - `library_root`：配合 output_dir 用 `url` 相对 root 还原目录结构；url 不在 root 下时回退目录名
/// - `config_dir`：`${configDir}` 占位符基准（=application.yml 所在目录）；无基准时占位符原样保留
/// - oneshot 时基准为 zip 的父目录与 zip 文件名（去扩展名）
/// 返回 (导出目录, oneshot 文件名基准 Option)。
pub fn resolve_mylar_output_dir(
    url: &str,
    oneshot: bool,
    output_dir: Option<&str>,
    library_root: Option<&str>,
    config_dir: Option<&std::path::Path>,
) -> (std::path::PathBuf, Option<String>) {
    let url_path = std::path::Path::new(url);
    let (series_dir, file_stem) = if oneshot {
        (
            url_path.parent().unwrap_or(url_path),
            url_path.file_stem().map(|s| s.to_string_lossy().to_string()),
        )
    } else {
        (url_path, None)
    };
    let dir = match output_dir {
        Some(out) => {
            // ${configDir} 展开为配置目录绝对路径，其余相对路径仍相对进程 cwd。
            let out_str = match config_dir {
                Some(dir) => out.replace("${configDir}", &dir.to_string_lossy()),
                None => out.to_string(),
            };
            let out = std::path::Path::new(&out_str);
            if let Some(root) = library_root {
                match series_dir.strip_prefix(std::path::Path::new(root)) {
                    Ok(rel) => out.join(rel),
                    Err(_) => out.join(
                        series_dir
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_default(),
                    ),
                }
            } else {
                out.join(
                    series_dir
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                )
            }
        }
        None => series_dir.to_path_buf(),
    };
    (dir, file_stem)
}

/// 写 series.json 到导出目录（自动创建目录）；oneshot → `<文件名基准>.oneshot.json`
/// （文件名基准由 `resolve_mylar_output_dir` 给出 = url 中 zip 的文件名）。
/// 返回写入的完整路径。序列化格式对齐 py：indent=2、非 ASCII 原样（ensure_ascii=False）、无尾换行。
pub fn write_series_json(
    dir: &Path,
    series_name: &str,
    oneshot: bool,
    json: &MylarSeriesJson,
) -> Result<std::path::PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("create dir {}: {e}", dir.display()))?;
    let file_name = if oneshot {
        format!("{series_name}.oneshot.json")
    } else {
        "series.json".to_string()
    };
    let path = dir.join(file_name);
    let body = serde_json::to_string_pretty(json).map_err(|e| e.to_string())?;
    std::fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use komf_core::model::{Author, ReleaseDate, SeriesTitle, WebLink};

    fn sample_series() -> SeriesMetadata {
        SeriesMetadata {
            title: Some(SeriesTitle {
                name: "それでも".to_string(),
                r#type: Some(TitleType::Native),
                language: None,
            }),
            titles: vec![
                SeriesTitle {
                    name: "それでも".to_string(),
                    r#type: Some(TitleType::Native),
                    language: None,
                },
                SeriesTitle {
                    name: "soredemo".to_string(),
                    r#type: Some(TitleType::Romaji),
                    language: None,
                },
                SeriesTitle {
                    name: "Even So".to_string(),
                    r#type: Some(TitleType::Localized),
                    language: Some("en".to_string()),
                },
            ],
            summary: Some("简介".to_string()),
            publisher: Some(komf_core::model::Publisher {
                name: "小学館".to_string(),
                r#type: None,
                language_tag: None,
            }),
            reading_direction: Some(ReadingDirection::RightToLeft),
            age_rating: Some(13),
            language: Some("ja".to_string()),
            genres: vec!["恋爱".to_string()],
            tags: vec!["tag1".to_string()],
            total_book_count: Some(3),
            authors: vec![Author {
                name: "青山剛昌".to_string(),
                role: AuthorRole::Writer,
            }],
            release_date: Some(ReleaseDate::new(Some(2024), Some(5), Some(1))),
            links: vec![WebLink {
                label: "bangumi".to_string(),
                url: "https://bgm.tv/subject/1".to_string(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn mylar_json_structure_matches_py() {
        let json = mylar_series_json_from_metadata(&sample_series(), 2);
        assert_eq!(json.version, "1.0.2");
        let m = &json.metadata;
        assert_eq!(m.r#type, "comicSeries");
        assert_eq!(m.name, "それでも");
        assert_eq!(m.publisher, "小学館");
        assert_eq!(m.comicid, 9527);
        assert_eq!(m.year, 2024); // releaseDate 年份覆盖占位
        assert_eq!(m.description_text, "简介");
        assert_eq!(m.booktype, "Print");
        assert_eq!(m.age_rating.as_deref(), Some("12+")); // 13 → 12+
        assert_eq!(m.total_issues, 3); // totalBookCount 优先
        assert_eq!(m.status, "Continuing");
        assert_eq!(m.language.as_deref(), Some("ja"));
        assert_eq!(m.reading_direction.as_deref(), Some("RIGHT_TO_LEFT"));
        assert_eq!(m.release_date.as_deref(), Some("2024-05-01"));
        assert_eq!(m.genres.as_deref(), Some(vec!["恋爱".to_string()].as_slice()));
        assert_eq!(m.tags.as_deref(), Some(vec!["tag1".to_string()].as_slice()));
        // links
        let links = m.links.as_ref().unwrap();
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].label, "bangumi");
        // authors（py 透传 Komga 聚合 authors → role 小写）
        let authors = m.authors.as_ref().unwrap();
        assert_eq!(authors[0].role, "writer");
        // alternateTitles：剔除主标题（Native 全等），保留 Romaji + Localized
        let alts = m.alternate_titles.as_ref().unwrap();
        assert_eq!(alts.len(), 2);
        assert_eq!(alts[0].label, "Romaji");
        assert_eq!(alts[0].title, "soredemo");
        assert_eq!(alts[1].label, "en"); // LOCALIZED → 语言
        assert_eq!(alts[1].title, "Even So");
        // 占位字段为 null / 空串
        assert!(m.imprint.is_none() && m.description_formatted.is_none() && m.volume.is_none() && m.collects.is_none());
        assert!(m.comic_image.is_empty() && m.publication_run.is_empty());
    }

    #[test]
    fn year_fallback_and_total_issues_fallback() {
        let mut s = sample_series();
        s.release_date = None;
        s.total_book_count = None;
        let json = mylar_series_json_from_metadata(&s, 2);
        assert_eq!(json.metadata.year, 2001); // 占位
        assert_eq!(json.metadata.total_issues, 2); // booksCount 回退
    }

    #[test]
    fn normalize_age_rating_matches_py() {
        assert_eq!(normalize_age_rating(None), None);
        assert_eq!(normalize_age_rating(Some(0)), Some("All".to_string()));
        assert_eq!(normalize_age_rating(Some(1)), Some("9+".to_string()));
        assert_eq!(normalize_age_rating(Some(11)), Some("9+".to_string()));
        assert_eq!(normalize_age_rating(Some(12)), Some("12+".to_string()));
        assert_eq!(normalize_age_rating(Some(14)), Some("12+".to_string()));
        assert_eq!(normalize_age_rating(Some(15)), Some("15+".to_string()));
        assert_eq!(normalize_age_rating(Some(16)), Some("15+".to_string()));
        assert_eq!(normalize_age_rating(Some(17)), Some("17+".to_string()));
        assert_eq!(normalize_age_rating(Some(18)), Some("Adult".to_string()));
        assert_eq!(normalize_age_rating(Some(100)), Some("Adult".to_string()));
    }

    #[test]
    fn status_mapping_matches_py() {
        assert_eq!(mylar_status(None), "Continuing");
        assert_eq!(mylar_status(Some(SeriesStatus::Ongoing)), "Continuing");
        assert_eq!(mylar_status(Some(SeriesStatus::Hiatus)), "Continuing");
        assert_eq!(mylar_status(Some(SeriesStatus::Abandoned)), "Continuing");
        assert_eq!(mylar_status(Some(SeriesStatus::Ended)), "Ended");
        assert_eq!(mylar_status(Some(SeriesStatus::Completed)), "Ended"); // Kotlin 对齐
    }

    #[test]
    fn resolve_output_dir_matches_py_semantics() {
        // 路径统一用组件构造（win_path），不依赖平台分隔符字面量：
        // 反斜杠在 Linux 上不是路径分隔符（单组件文件名 → parent() 为空 / strip_prefix 失败）。
        let win_path = |comps: &[&str]| {
            let mut b = std::path::PathBuf::new();
            for c in comps {
                b.push(c);
            }
            b.to_string_lossy().to_string()
        };
        let lib = win_path(&["E:", "lib"]);
        let series_a = win_path(&["E:", "lib", "series-a"]);
        let oneshot_zip = win_path(&["E:", "lib", "oneshot", "AAA.zip"]);
        let oneshot_dir = win_path(&["E:", "lib", "oneshot"]);
        let sub_series_b = win_path(&["E:", "lib", "sub", "series-b"]);
        let out = win_path(&["D:", "mylar"]);
        let other_c = win_path(&["F:", "other", "series-c"]);

        // 普通系列：无 output → 原目录
        let (dir, stem) = resolve_mylar_output_dir(&series_a, false, None, None, None);
        assert_eq!(dir, std::path::Path::new(&series_a));
        assert!(stem.is_none());
        // oneshot：无 output → zip 父目录 + zip stem
        let (dir, stem) = resolve_mylar_output_dir(&oneshot_zip, true, None, None, None);
        assert_eq!(dir, std::path::Path::new(&oneshot_dir));
        assert_eq!(stem.as_deref(), Some("AAA"));
        // 普通系列 + output → output/目录名
        let (dir, _) = resolve_mylar_output_dir(&series_a, false, Some(&out), None, None);
        assert_eq!(dir, std::path::Path::new(&win_path(&["D:", "mylar", "series-a"])));
        // 普通系列 + output + root → output/相对结构
        let (dir, _) = resolve_mylar_output_dir(&sub_series_b, false, Some(&out), Some(&lib), None);
        assert_eq!(dir, std::path::Path::new(&win_path(&["D:", "mylar", "sub", "series-b"])));
        // oneshot + output + root → output/zip 父目录相对结构 + stem
        let (dir, stem) = resolve_mylar_output_dir(&oneshot_zip, true, Some(&out), Some(&lib), None);
        assert_eq!(dir, std::path::Path::new(&win_path(&["D:", "mylar", "oneshot"])));
        assert_eq!(stem.as_deref(), Some("AAA"));
        // url 不在 root 下 → 回退目录名
        let (dir, _) = resolve_mylar_output_dir(&other_c, false, Some(&out), Some(&lib), None);
        assert_eq!(dir, std::path::Path::new(&win_path(&["D:", "mylar", "series-c"])));
        // ${configDir} 展开为配置目录；无基准时原样保留
        // 注意：Windows 上 PathBuf::from("C:").join("app") = "C:app"（相对盘符语义），
        // 必须用正斜杠字面量构造（两个平台都解析为 C:\app\conf / C:/app/conf）。
        let cfg = std::path::Path::new("C:/app/conf").to_path_buf();
        let (dir, _) = resolve_mylar_output_dir(
            &series_a,
            false,
            Some("${configDir}/mylar"),
            None,
            Some(&cfg),
        );
        assert_eq!(dir, std::path::Path::new("C:/app/conf/mylar/series-a"));
        let (dir, _) = resolve_mylar_output_dir(
            &series_a,
            false,
            Some("${configDir}/mylar"),
            None,
            None,
        );
        assert_eq!(dir, std::path::Path::new("${configDir}/mylar/series-a"));
    }

    #[test]
    fn write_series_json_names() {
        let dir = std::env::temp_dir().join(format!("komf-mylar-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let json = mylar_series_json_from_metadata(&sample_series(), 0);
        let p1 = write_series_json(&dir, "Series A", false, &json).unwrap();
        assert_eq!(p1.file_name().unwrap().to_str().unwrap(), "series.json");
        let p2 = write_series_json(&dir, "Series B", true, &json).unwrap();
        assert_eq!(p2.file_name().unwrap().to_str().unwrap(), "Series B.oneshot.json");
        let body = std::fs::read_to_string(&p1).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["version"], "1.0.2");
        assert_eq!(parsed["metadata"]["name"], "それでも"); // 非 ASCII 原样
        assert!(!body.ends_with('\n')); // 无尾换行（py 一致）
        let _ = std::fs::remove_dir_all(&dir);
    }
}
