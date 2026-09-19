//! 临时联调：验证 yenpress/viz/webtoons 三个新移植 provider。
//! 用法：`cargo run -p komf-core --example providers_new -- "Sword Art Online"`
use komf_core::config::ProviderConfig;
use komf_core::model::{MatchQuery, ProviderSeriesId};
use komf_core::providers::MetadataProvider;
use komf_core::util::NameSimilarityMatcher;

#[tokio::main]
async fn main() {
    let search = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "One Piece".to_string());
    let http = reqwest::Client::builder()
        .user_agent("dyphire/komf-rs (https://github.com/dyphire/komf-rs)")
        .build()
        .unwrap();
    let matcher = NameSimilarityMatcher::default();

    let providers: Vec<(&str, Option<Box<dyn MetadataProvider>>)> = vec![
        (
            "YenPress",
            komf_core::providers::yenpress::create_provider(
                &ProviderConfig {
                    priority: 1,
                    enabled: true,
                    ..Default::default()
                },
                matcher,
                &http,
            )
            .map(|p| Box::new(p) as Box<dyn MetadataProvider>),
        ),
        (
            "Viz",
            komf_core::providers::viz::create_provider(
                &ProviderConfig {
                    priority: 1,
                    enabled: true,
                    ..Default::default()
                },
                matcher,
                &http,
            )
            .map(|p| Box::new(p) as Box<dyn MetadataProvider>),
        ),
        (
            "Webtoons",
            komf_core::providers::webtoons::create_provider(
                &ProviderConfig {
                    priority: 1,
                    enabled: true,
                    ..Default::default()
                },
                matcher,
                &http,
            )
            .map(|p| Box::new(p) as Box<dyn MetadataProvider>),
        ),
    ];

    for (name, p) in providers {
        let Some(p) = p else {
            println!("{name}: CREATE_FAILED");
            continue;
        };
        match p.search_series(&search, 5, None).await {
            Ok(hits) => {
                println!("{name}: OK {} hits", hits.len());
                for h in hits.iter().take(3) {
                    println!("    - {} | {} | url={:?}", h.result_id, h.title, h.url);
                }
                if let Some(first) = hits.first() {
                    let id = ProviderSeriesId(first.result_id.clone());
                    match p.get_series_metadata(&id).await {
                        Ok(meta) => {
                            let t = meta.metadata.titles.first().map(|t| t.name.clone());
                            let books = meta.books.len();
                            println!(
                                "    metadata: title={:?} books={} authors={} status={:?} summary_len={}",
                                t,
                                books,
                                meta.metadata.authors.len(),
                                meta.metadata.status,
                                meta.metadata.summary.as_ref().map(|s| s.len()).unwrap_or(0)
                            );
                            if let Some(book) = meta.books.first() {
                                match p.get_book_metadata(&id, &book.id).await {
                                    Ok(bm) => println!(
                                        "    book0: title={:?} number={:?} rel={:?} links={}",
                                        bm.metadata.title,
                                        bm.metadata.number,
                                        bm.metadata.release_date,
                                        bm.metadata.links.len()
                                    ),
                                    Err(e) => println!("    book0 ERR: {e}"),
                                }
                            }
                        }
                        Err(e) => println!("    metadata ERR: {e}"),
                    }
                }
                let q = MatchQuery::new(search.clone(), None, None, None);
                match p.match_series_metadata(&q).await {
                    Ok(Some(m)) => println!(
                        "    match: OK title={:?}",
                        m.metadata.titles.first().map(|t| t.name.clone())
                    ),
                    Ok(None) => println!("    match: no match"),
                    Err(e) => println!("    match ERR: {e}"),
                }
            }
            Err(e) => println!("{name}: ERR {e}"),
        }
    }
}
