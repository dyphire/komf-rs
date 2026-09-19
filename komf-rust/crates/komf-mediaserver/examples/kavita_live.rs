//! Kavita 真实实例联调冒烟（非交付物，仅本机验证用）。
//! 运行：cargo run -p komf-mediaserver --example kavita_live
//! 环境变量：KAVITA_BASE（默认 http://127.0.0.1:5000）、KAVITA_API_KEY（必填）

use komf_mediaserver::kavita::KavitaClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let base = std::env::var("KAVITA_BASE").unwrap_or_else(|_| "http://127.0.0.1:5000".to_string());
    let key = std::env::var("KAVITA_API_KEY").expect("KAVITA_API_KEY env required");
    let http = reqwest::Client::builder().build()?;
    let client = KavitaClient::new(http, &base, &key)?;

    // 1. authenticate 隐含 + libraries
    let libs = client.get_libraries().await?;
    println!("[1] libraries: {} 个", libs.len());
    for lib in &libs {
        println!("    id={} name={} type={} folders={:?}", lib.id, lib.name, lib.r#type, lib.folders);
    }

    // 2. 第一个库的分页系列（series/v2 statements field=19）
    if let Some(lib) = libs.first() {
        let (series, pagination) = client.get_series_page(lib.id, 0).await?;
        println!(
            "[2] 库[{}] page0: {} 条, pagination={:?}",
            lib.name,
            series.len(),
            pagination
        );
        // 3. 第一系列的详情 / 元数据 / 卷
        if let Some(s) = series.first() {
            println!("[3] series: id={} name={} sort={} pages={} format={}", s.id, s.name, s.sort_name, s.pages, s.format);
            let details = client.get_series_details(s.id).await?;
            println!("    details: total_count={} volumes={:?}", details.total_count, details.volumes.as_ref().map(|v| v.len()));
            let metadata = client.get_series_metadata(s.id).await?;
            println!(
                "    metadata: id={} summary_len={} genres={} writers={} coverArtists={} tags={} ageRating={}",
                metadata.id,
                metadata.summary.as_ref().map(|s| s.len()).unwrap_or(0),
                metadata.genres.len(),
                metadata.writers.len(),
                metadata.cover_artists.len(),
                metadata.tags.len(),
                metadata.age_rating,
            );
            // 4. 卷与章节
            let volumes = client.get_volumes(s.id).await?;
            println!("    volumes: {} 个", volumes.len());
            for v in volumes.iter().take(3) {
                println!("      volume id={} min={} max={} name={} pages={} chapters={}", v.id, v.min_number, v.max_number, v.name, v.pages, v.chapters.len());
                if let Some(ch) = v.chapters.first() {
                    println!("        chapter id={} title={} number={:?} pages={} created={}", ch.id, ch.title, ch.number, ch.pages, ch.created_utc);
                    let chap = client.get_chapter(ch.id).await?;
                    println!("        get_chapter -> id={} title={} files={}", chap.id, chap.title, chap.files.len());
                }
            }
            // 5. 封面下载（apiKey 查询参数）
            let cover = client.get_series_cover(s.id).await?;
            println!("[5] series_cover: {} bytes, mime={:?}", cover.bytes.len(), cover.mime_type);
        }
    }

    // 6. 单系列获取（api/series/{id}）
    if let Some(lib) = libs.first() {
        let (series, _) = client.get_series_page(lib.id, 0).await?;
        if let Some(s) = series.first() {
            let direct = client.get_series(s.id).await?;
            println!("[6] get_series({}) -> name={} folder={}", s.id, direct.name, direct.folder_path);
        }
    }

    println!("KAVITA LIVE OK");
    Ok(())
}
