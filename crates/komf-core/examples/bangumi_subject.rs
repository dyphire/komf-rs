//! 测试 bangumi provider 对 bgm.tv/subject/412571 的元数据解析结果。
//!
//! 用法：`cargo run -p komf-core --example bangumi_subject`
//! 链路：resolve_link_id → resolve_link_search_result → get_series_metadata
//!       → 首个单行本 get_book_metadata → match_series_metadata
use komf_core::config::{BangumiArchiveConfig, BangumiConfig, ProviderConfig};
use komf_core::model::{MatchQuery, ProviderSeriesId};
use komf_core::providers::MetadataProvider;
use komf_core::util::NameSimilarityMatcher;

const SUBJECT_URL: &str = "https://bgm.tv/subject/412571";
const SUBJECT_ID: &str = "412571";

#[tokio::main]
async fn main() {
    let http = reqwest::Client::builder()
        .user_agent("dyphire/komf-rs (https://github.com/dyphire/komf-rs)")
        .build()
        .unwrap();
    let matcher = NameSimilarityMatcher::default();

    let p = komf_core::providers::bangumi::create_provider(
        &BangumiConfig {
            provider: ProviderConfig {
                priority: 1,
                enabled: true,
                ..Default::default()
            },
            archive: BangumiArchiveConfig::default(),
        },
        matcher,
        None,
        &http,
        None,
        None, // archive（全局服务由 ProvidersModule 创建；example 走在线 API）
        None,
    )
    .expect("bangumi provider create failed");

    // 1) 链接 → ID 解析
    println!("== [1] resolve_link_id ==");
    println!("query: {SUBJECT_URL}");
    match p.resolve_link_id(SUBJECT_URL) {
        Some(id) => println!("resolved id: {id}"),
        None => println!("NONE (链接未被识别)"),
    }

    // 2) 链接 → 搜索结果
    println!("\n== [2] resolve_link_search_result ==");
    match p.resolve_link_search_result(SUBJECT_URL).await {
        Some(sr) => println!(
            "{}",
            serde_json::to_string_pretty(&sr).unwrap_or_else(|e| format!("serialize ERR: {e}"))
        ),
        None => {
            println!("NONE (未能解析)");
            // 直接调用底层 client 定位原因
            println!("\n== [2b] BangumiClient::get(412571) 详情 ==");
            let client = komf_core::providers::bangumi::BangumiClient::new(http.clone(), None);
            match client.get(412571).await {
                Ok(s) => println!("OK: id={} name={:?} name_cn={:?}", s.id, s.name, s.name_cn),
                Err(e) => println!("ERR: {e:?}"),
            }
        }
    }

    // 3) 系列元数据
    println!("\n== [3] get_series_metadata ==");
    let id = ProviderSeriesId(SUBJECT_ID.to_string());
    let meta = match p.get_series_metadata(&id).await {
        Ok(m) => m,
        Err(e) => {
            println!("ERR: {e}");
            return;
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&meta).unwrap_or_else(|e| format!("serialize ERR: {e}"))
    );

    // 4) 首个单行本元数据
    if let Some(book) = meta.books.first() {
        println!("\n== [4] get_book_metadata (books[0] = {}) ==", book.id.0);
        match p.get_book_metadata(&id, &book.id).await {
            Ok(bm) => println!(
                "{}",
                serde_json::to_string_pretty(&bm).unwrap_or_else(|e| format!("serialize ERR: {e}"))
            ),
            Err(e) => println!("ERR: {e}"),
        }
    } else {
        println!("\n== [4] get_book_metadata ==");
        println!("SKIP (无单行本)");
    }

    // 5) 名称匹配（日文名 + 中文名 + 别名 + 版本名——版本名仅用于匹配）
    for name in [
        "ペンと手錠と事実婚",
        "笔、手铐和事实婚",
        "笔与手铐与事实婚姻",
        "筆、手銬和事實婚",
    ] {
        println!("\n== [5] match_series_metadata ({name}) ==");
        match p
            .match_series_metadata(&MatchQuery::new(name.to_string(), None, None, None))
            .await
        {
            Ok(Some(m)) => println!(
                "MATCH: id={} title={:?} score-based hit",
                m.id.0,
                m.metadata.titles.first().map(|t| t.name.clone())
            ),
            Ok(None) => println!("no match"),
            Err(e) => println!("ERR: {e}"),
        }
    }
}
