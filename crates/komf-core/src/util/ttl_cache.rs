//! 通用 TTL 缓存 —— 对应 Kotlin cache4k `expireAfterWrite` 语义
//! （`Cache.Builder.expireAfterWrite(duration)`：写入后固定时长过期，命中未过期直接返回）。
//!
//! 合并自 ehentai / mangabaka / webtoons 三处逐字相同的本地拷贝
//! （Kotlin 原版没有独立组件，Rust 侧最初在 EHentaiMetadataProvider 中引入，
//! 后复制到 MangaBaka / Webtoons provider）：
//! - `get_or_load`：双检锁 —— 命中且未过期直接返回；未命中执行 load，**仅成功结果入缓存**
//!   （失败不缓存，对齐 cache4k `load` 的 through-loading 行为）；
//! - `put`：手动写入（MangaBaka resolve_link_search_result 用）；
//! - `insert_limited`：capacity 为 Rust 自定的软上限（Kotlin 未配置 maximumSize），
//!   插入前先清过期项，仍超限则按写入时间淘汰最旧的 excess 条（近似 LRU）。
//!
//! 容量只是软上限，HashMap 无序，淘汰顺序不影响正确性。

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// 简单 TTL 缓存 —— 对应 Kotlin cache4k `expireAfterWrite`。
pub struct TtlCache<K, V> {
    inner: tokio::sync::Mutex<HashMap<K, (V, Instant)>>,
    ttl: Duration,
    capacity: usize,
}

impl<K, V> TtlCache<K, V>
where
    K: Eq + std::hash::Hash + Clone,
    V: Clone,
{
    pub fn new(ttl: Duration) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(HashMap::new()),
            ttl,
            capacity: 10_000,
        }
    }

    /// 命中且未过期直接返回；未命中执行 load，仅成功结果入缓存。
    pub async fn get_or_load<E, F, Fut>(&self, key: K, load: F) -> Result<V, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<V, E>>,
    {
        {
            let guard = self.inner.lock().await;
            if let Some((value, created)) = guard.get(&key) {
                if created.elapsed() < self.ttl {
                    return Ok(value.clone());
                }
            }
        }
        let value = load().await?;
        let mut guard = self.inner.lock().await;
        self.insert_limited(&mut guard, key, value.clone());
        Ok(value)
    }

    pub async fn put(&self, key: K, value: V) {
        let mut guard = self.inner.lock().await;
        self.insert_limited(&mut guard, key, value);
    }

    /// 插入并维持容量上限：先清过期项；仍超限则移除最旧的 excess 条（近似 LRU，
    /// 与 cache4k 的"超限淘汰最旧"语义一致）。HashMap 无序，按写入时间排序取
    /// 最早的 `excess` 个即可——容量只是软上限，淘汰顺序不影响正确性。
    fn insert_limited(&self, guard: &mut HashMap<K, (V, Instant)>, key: K, value: V) {
        if guard.len() >= self.capacity {
            let now = Instant::now();
            guard.retain(|_, (_, created)| now.duration_since(*created) < self.ttl);
        }
        guard.insert(key, (value, Instant::now()));
        if guard.len() > self.capacity {
            let excess = guard.len() - self.capacity;
            let oldest: Vec<K> = {
                let mut entries: Vec<(&K, &Instant)> =
                    guard.iter().map(|(k, (_, c))| (k, c)).collect();
                entries.sort_by_key(|(_, c)| **c);
                entries
                    .into_iter()
                    .take(excess)
                    .map(|(k, _)| k.clone())
                    .collect()
            };
            for k in oldest {
                guard.remove(&k);
            }
        }
    }
}
