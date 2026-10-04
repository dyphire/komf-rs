//! SSE 帧解析共享工具 —— Komga 事件流（`event_listener.rs`）与 Kavita SignalR
//! SSE transport（`kavita_signalr.rs`）使用同一套帧边界/字段解析逻辑
//! （对应 Kotlin 两侧各自重复的 SSE frame 处理代码）。

/// 查找一帧 SSE 的边界（`\n\n`，兼容 `\r\n\r\n`）。
/// 注意匹配顺序：`windows(2)` 的 `\n\n` 优先于 `windows(4)` 的 `\r\n\r\n`
/// （`\r\n\r\n` 内包含 `\n\r`，不存在 `\n\n` 子串，两种顺序结果一致，
/// 但保持原实现顺序以避免歧义）。
pub(crate) fn find_frame_boundary(buffer: &[u8]) -> Option<usize> {
    buffer
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|pos| pos + 2)
        .or_else(|| {
            buffer
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|pos| pos + 4)
        })
}

/// 解析一帧 SSE：提取 `event:` 与 `data:` 行。
/// 返回 `(event 名（可无）, data)`；缺少 `data:` 行时返回 None。
pub(crate) fn parse_sse_frame(frame: &[u8]) -> Option<(Option<String>, String)> {
    let text = String::from_utf8_lossy(frame);
    let mut event_type: Option<String> = None;
    let mut data: Option<String> = None;
    for line in text.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("event:") {
            event_type = Some(rest.trim().to_string());
            continue;
        }
        if let Some(rest) = line.strip_prefix("data:") {
            data = Some(rest.trim_start().to_string());
        }
    }
    data.map(|data| (event_type, data))
}
