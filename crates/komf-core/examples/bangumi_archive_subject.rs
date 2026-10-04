//! 测试 bangumi 离线数据库（Archive）对 bgm.tv/subject/412571 的查询解析结果。
//!
//! 用法：`cargo run -p komf-core --example bangumi_archive_subject`
//! 链路：直接打开离线 store 验证原始数据 → provider(archive enabled)
//!       → resolve_link_search_result → get_series_metadata → get_book_metadata
use komf_core::config::{BangumiArchiveConfig, BangumiConfig, ProviderConfig};
use komf_core::model::{ProviderBookId, ProviderSeriesId};
use komf_core::providers::MetadataProvider;
use komf_core::util::NameSimilarityMatcher;
use std::path::Path;
use std::time::Duration;

const SUBJECT_URL: &str = "https://bgm.tv/subject/412571";
const ARCHIVE_DIR: &str = "G:/Github/komf/target/bangumi-archive";

fn compact_thumbnail(v: &mut serde_json::Value) {
    let Some(tb) = v.get_mut("metadata").and_then(|x| x.get_mut("thumbnail")) else {
        return;
    };
    let Some(obj) = tb.as_object_mut() else {
        return;
    };
    if let Some(bytes) = obj.get("bytes").and_then(|b| b.as_array()) {
        let n = bytes.len();
        obj.remove("bytes");
        obj.insert("_bytes_omitted".to_string(), serde_json::json!(n));
    }
}

#[tokio::main]
async fn main() {
    let http = reqwest::Client::builder()
        .user_agent("dyphire/komf-rs (https://github.com/dyphire/komf-rs)")
        .build()
        .unwrap();
    let matcher = NameSimilarityMatcher::default();

    // [0] 直接打开离线 store，验证 412571 在离线库中的原始数据
    println!("== [0] 离线库原始数据（BangumiArchiveStore::open 直查）==");
    let db = Path::new(ARCHIVE_DIR).join("archive_index.db");
    match komf_core::providers::bangumi_archive::BangumiArchiveStore::open(&db) {
        Ok(store) => {
            match store.get_by_id(412571) {
                Some(v) => {
                    let arch: komf_core::providers::bangumi_archive::ArchiveSubject =
                        serde_json::from_value(v).unwrap_or_default();
                    println!(
                        "subject: id={} name={:?} name_cn={:?} type={:?} platform={:?} series={:?} date={:?} nsfw={:?}",
                        arch.id, arch.name, arch.name_cn, arch.subject_type, arch.platform, arch.series, arch.date, arch.nsfw
                    );
                    println!(
                        "         tags={:?} infobox_len={}",
                        arch.tags
                            .iter()
                            .map(|t| t.name.as_str())
                            .collect::<Vec<_>>(),
                        arch.infobox
                            .as_ref()
                            .map(|i| i.to_string().len())
                            .unwrap_or(0)
                    );
                }
                None => println!("NOT FOUND in offline archive!"),
            }
            let rel = store.get_related(412571);
            println!(
                "related {}: {:?}",
                rel.len(),
                rel.iter()
                    .filter(|r| r.subject_type == Some(1) && r.relation.as_deref() == Some("单行本"))
                    .map(|r| (r.id, r.relation.as_deref().unwrap_or(""), r.name.as_str()))
                    .collect::<Vec<_>>()
            );
            let persons = store.get_persons(412571);
            println!(
                "persons {}: {:?}",
                persons.len(),
                persons
                    .iter()
                    .map(|p| (
                        p.name.as_str(),
                        p.name_cn.as_deref().unwrap_or(""),
                        p.career.join("/")
                    ))
                    .collect::<Vec<_>>()
            );
        }
        Err(e) => println!("store open ERR: {e}"),
    }

    // provider（archive enabled）：全局服务现由 ProvidersModule 创建，example 自行创建后传入
    let archive = Some(
        komf_core::providers::bangumi_archive::BangumiArchiveService::start(
            &BangumiArchiveConfig {
                enabled: true,
                dir: Some(ARCHIVE_DIR.to_string()),
                update_interval_hours: 0,
                idle_release_secs: None,
            },
            http.clone(),
            std::path::PathBuf::from(ARCHIVE_DIR),
        ),
    );
    let p = komf_core::providers::bangumi::create_provider(
        &BangumiConfig {
            provider: ProviderConfig {
                priority: 1,
                enabled: true,
                ..Default::default()
            },
            archive: BangumiArchiveConfig {
                enabled: true,
                dir: Some(ARCHIVE_DIR.to_string()),
                update_interval_hours: 0,
                idle_release_secs: None,
            },
        },
        matcher,
        None,
        &http,
        None,
        None,
        archive,
        Some("zh".to_string()),
    )
    .expect("provider create failed");

    // 等待后台服务加载本地索引（235MB db + mmap 967MB jsonlines）
    println!("\nwaiting 8s for archive service to load existing index...");
    tokio::time::sleep(Duration::from_secs(8)).await;

    // [1] 离线 resolve_link_search_result
    println!("\n== [1] resolve_link_search_result (archive 路径) ==");
    match p.resolve_link_search_result(SUBJECT_URL).await {
        Some(sr) => println!(
            "{}",
            serde_json::to_string_pretty(&sr).unwrap_or_else(|e| format!("serialize ERR: {e}"))
        ),
        None => println!("NONE"),
    }

    // [2] 离线 get_series_metadata
    println!("\n== [2] get_series_metadata (archive 路径) ==");
    let id = ProviderSeriesId("412571".to_string());
    match p.get_series_metadata(&id).await {
        Ok(m) => {
            let mut mj = serde_json::to_value(&m).unwrap();
            compact_thumbnail(&mut mj);
            println!("{}", serde_json::to_string_pretty(&mj).unwrap());
        }
        Err(e) => println!("ERR: {e}"),
    }

    // [3] 离线 get_book_metadata（第 1 卷 450936）
    println!("\n== [3] get_book_metadata(450936, archive 路径) ==");
    let book_id = ProviderBookId("450936".to_string());
    match p.get_book_metadata(&id, &book_id).await {
        Ok(bm) => {
            let mut bj = serde_json::to_value(&bm).unwrap();
            compact_thumbnail(&mut bj);
            println!("{}", serde_json::to_string_pretty(&bj).unwrap());
        }
        Err(e) => println!("ERR: {e}"),
    }
}
