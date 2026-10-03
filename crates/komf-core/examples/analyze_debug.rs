//! 临时调试：打印指定标题的分析链输出
fn main() {
    let title = "[オンキュウ] 生意気ギャルがわからせられる本3.0 [中国翻訳] [DL版]";
    println!("search_analyze:");
    for t in komf_core::util::search_analyze(title) {
        println!("  {t:?}");
    }
    println!("index_analyze_terms:");
    let joined = komf_core::util::index_analyze_terms(title).join(" ");
    println!("  {joined}");
    println!("query tokens for 生意気ギャルがわからせられる本3.0:");
    for t in komf_core::util::search_analyze("生意気ギャルがわからせられる本3.0") {
        println!("  {t:?}");
    }
}
