//! 封面图片降级工具 —— 对齐 KomgaBangumi.user.js 的「过大降级 / 413 降级」语义。
//!
//! 脚本（updateKomgaBookCover / updateKomgaSeriesCover）对过大（≥1MB）或上传
//! 返回 413 的封面，会换用更小尺寸的源图重试。komf 的 provider 只产出单个
//! `Image`，等价做法是缩放重编码为 JPEG 后重试。
//!
//! 缩放策略：**按目标字节数比例缩放**（像素面积 ∝ 字节数），而非机械减半——
//! 减半在"只略超限制"时会造成不必要的质量损失（2000px 图缩到 1000px 才能省一半）。

use komf_core::model::Image;
use std::io::Cursor;

/// 降级轮数上限（对应脚本最多尝试「首选/中/通用/较小」四档）。
pub const MAX_DOWNSCALE_ROUNDS: u32 = 3;

/// 书籍封面最小有效字节数（对齐脚本 updateKomgaBookCover 的 <30kB 跳过）。
pub const BOOK_MIN_THUMBNAIL_SIZE: u64 = 30 * 1024;
/// 系列封面最小有效字节数（对齐脚本 updateKomgaSeriesCover 的 <60kB 跳过）。
pub const SERIES_MIN_THUMBNAIL_SIZE: u64 = 60 * 1024;

/// 按目标字节数缩放：像素比例 = sqrt(target/current) × 保险系数 0.9，
/// 保证一次到位且略低于目标（JPEG 字节数与像素面积近似线性）。
/// 已 ≤ target 时原样返回；无法解码/编码返回 `None`。
pub fn resize_towards(image: &Image, target_bytes: u64) -> Option<Image> {
    let current = image.bytes.len() as u64;
    if current <= target_bytes {
        return Some(image.clone());
    }
    let mut reader = image::ImageReader::new(Cursor::new(&image.bytes))
        .with_guessed_format()
        .ok()?;
    let decoded = reader.decode().ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    // 保险系数 0.9 避免重编码后（JPEG 质量波动）仍略超目标；下限 5% 防除零/极端缩小。
    let ratio = ((target_bytes as f64 / current as f64).sqrt() * 0.9).max(0.05);
    let nw = ((w as f64 * ratio).round().max(1.0)) as u32;
    let nh = ((h as f64 * ratio).round().max(1.0)) as u32;
    if nw >= w || nh >= h {
        return None;
    }
    let resized = decoded.resize(nw, nh, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    resized
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
        .ok()?;
    Some(Image::new(out, Some("image/jpeg".to_string())))
}

/// 把图片最长边减半并重编码为 JPEG（保留：用于显式二分场景）。
/// 返回 `None` 表示无法解码/编码（调用方按原逻辑跳过或报错）。
pub fn downscale_image(image: &Image) -> Option<Image> {
    let mut reader = image::ImageReader::new(Cursor::new(&image.bytes))
        .with_guessed_format()
        .ok()?;
    let decoded = reader.decode().ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    // 已无法继续缩小。
    if w <= 2 && h <= 2 {
        return None;
    }
    let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
    let resized = decoded.resize(nw, nh, image::imageops::FilterType::Lanczos3);
    let mut out = Vec::new();
    resized
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
        .ok()?;
    Some(Image::new(out, Some("image/jpeg".to_string())))
}

/// 若字节数超出 `limit`，按目标字节比例逐轮缩放直到 ≤ limit。
/// 无法解码或已无法再缩小但仍旧超限时，返回最后一个可达版本（由调用方决定跳过或报错）。
pub fn ensure_within_limit(image: &Image, limit: u64) -> Image {
    if image.bytes.len() as u64 <= limit {
        return image.clone();
    }
    let mut current = image.clone();
    for _ in 0..MAX_DOWNSCALE_ROUNDS {
        match resize_towards(&current, limit) {
            Some(next) if next.bytes.len() as u64 <= limit => return next,
            Some(next) => current = next,
            None => break,
        }
    }
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 生成一张已知尺寸的 JPEG 用于测试。
    fn make_jpeg(width: u32, height: u32) -> Image {
        let img = image::RgbImage::from_pixel(
            width,
            height,
            image::Rgb([200u8, 60u8, 90u8]),
        );
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Jpeg)
            .expect("encode jpeg");
        Image::new(out, Some("image/jpeg".to_string()))
    }

    #[test]
    fn downscale_halves_longest_side_and_reencodes_jpeg() {
        let img = make_jpeg(800, 400);
        let down = downscale_image(&img).expect("downscale should succeed");
        // JPEG 重编码：尺寸减半，mime 变回 jpeg。
        let reader = image::ImageReader::new(Cursor::new(&down.bytes))
            .with_guessed_format()
            .expect("decode downscaled");
        let decoded = reader.decode().expect("decode");
        assert_eq!(decoded.width(), 400);
        assert_eq!(decoded.height(), 200);
        assert_eq!(down.mime_type.as_deref(), Some("image/jpeg"));
    }

    #[test]
    fn downscale_tiny_image_returns_none() {
        let img = make_jpeg(2, 2);
        assert!(downscale_image(&img).is_none());
    }

    #[test]
    fn downscale_invalid_bytes_returns_none() {
        let img = Image::new(vec![0u8, 1, 2, 3], Some("image/jpeg".to_string()));
        assert!(downscale_image(&img).is_none());
    }

    #[test]
    fn ensure_within_limit_keeps_small_image_unchanged() {
        let img = make_jpeg(64, 64);
        assert_eq!(ensure_within_limit(&img, u64::MAX).bytes, img.bytes);
    }

    #[test]
    fn ensure_within_limit_downscales_large_image() {
        let img = make_jpeg(2000, 2000);
        let fitted = ensure_within_limit(&img, 200_000);
        // 2000x2000 JPEG 通常 > 200kB；降级后应 ≤ limit。
        assert!(
            (fitted.bytes.len() as u64) <= 200_000,
            "fitted size {} should be within limit",
            fitted.bytes.len()
        );
    }

    #[test]
    fn resize_towards_returns_same_when_within_target() {
        let img = make_jpeg(64, 64);
        let out = resize_towards(&img, 1024 * 1024).expect("within target returns clone");
        assert_eq!(out.bytes, img.bytes);
    }

    #[test]
    fn resize_towards_scales_proportionally_not_halving() {
        // 1200x1200 略超 600kB 目标：按比例应缩到 ~1000px 左右（保留更多质量），
        // 而不是机械减半到 600px。
        let img = make_jpeg(1200, 1200);
        let target = (img.bytes.len() as u64) * 70 / 100; // 目标 = 当前字节的 70%
        let out = resize_towards(&img, target).expect("resize should succeed");
        let reader = image::ImageReader::new(Cursor::new(&out.bytes))
            .with_guessed_format()
            .expect("decode");
        let decoded = reader.decode().expect("decode");
        // 比例 √0.7×0.9 ≈ 0.75 → 1200×0.75 ≈ 900；减半则是 600。断言明显大于减半结果。
        assert!(
            decoded.width() > 700,
            "proportional resize should keep more quality than halving, got width {}",
            decoded.width()
        );
        assert!(
            (out.bytes.len() as u64) <= target,
            "resized size {} should be <= target {}",
            out.bytes.len(),
            target
        );
    }

    #[test]
    fn resize_towards_invalid_bytes_returns_none() {
        // target 小于 current 才会触发解码路径；非法字节解码失败 → None。
        let img = Image::new(vec![0u8, 1, 2, 3], Some("image/jpeg".to_string()));
        assert!(resize_towards(&img, 1).is_none());
    }
}
