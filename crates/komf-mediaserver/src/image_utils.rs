//! 封面图片降级工具 —— 对齐 KomgaBangumi.user.js 的「过大降级 / 413 降级」语义。
//!
//! 脚本（updateKomgaBookCover / updateKomgaSeriesCover）对过大（≥1MB）或上传
//! 返回 413 的封面，会换用更小尺寸的源图重试。komf 的 provider 只产出单个
//! `Image`，等价做法是缩放重编码为 JPEG 后重试。
//!
//! 缩放策略：按目标字节数比例缩放（像素面积 ∝ 字节数），而非机械减半。
//! 这样在图片只略超限制时，可以尽量保留原始分辨率，避免不必要的质量损失。
//!
//! 注意：JPEG 重编码后的实际字节数无法仅由像素面积精确预测，因此每轮都会
//! 实际检查输出大小；如果仍超过限制，则继续降级，最多 MAX_DOWNSCALE_ROUNDS 轮。

use komf_core::model::Image;
use std::io::Cursor;

/// 降级轮数上限。
///
/// 原脚本最多可以在首选源之外尝试若干更小的版本；这里用动态缩放模拟。
pub const MAX_DOWNSCALE_ROUNDS: u32 = 3;

/// 书籍封面最小有效字节数。
///
/// 对齐脚本 updateKomgaBookCover 的 <30kB 跳过逻辑。
pub const BOOK_MIN_THUMBNAIL_SIZE: u64 = 30 * 1024;

/// 系列封面最小有效字节数。
///
/// 对齐脚本 updateKomgaSeriesCover 的 <60kB 跳过逻辑。
pub const SERIES_MIN_THUMBNAIL_SIZE: u64 = 60 * 1024;

/// JPEG 缩放时的安全系数。
///
/// JPEG 文件大小与像素面积大致相关，但不是严格线性关系，因此在理论
/// 缩放比例上额外乘以 0.9，为重编码后的大小波动预留空间。
const RESIZE_SAFETY_FACTOR: f64 = 0.9;

/// 最小缩放比例。
///
/// 防止极端情况下 target/current 非常小，导致一次缩放直接变成 1×1。
const MIN_RESIZE_RATIO: f64 = 0.05;

/// 按目标字节数比例缩放并重编码为 JPEG。
///
/// 像素面积与目标文件大小近似成正比，因此：
///
/// ```text
/// width_ratio = sqrt(target_bytes / current_bytes)
/// height_ratio = sqrt(target_bytes / current_bytes)
/// ```
///
/// 再乘以 `RESIZE_SAFETY_FACTOR`，避免重编码后仍略微超过目标。
///
/// 如果当前图片已经不超过 `target_bytes`，直接返回原图副本。
///
/// 返回 `None` 表示：
///
/// - 无法识别图片格式
/// - 无法解码
/// - 无法进一步缩小
/// - JPEG 重编码失败
pub fn resize_towards(image: &Image, target_bytes: u64) -> Option<Image> {
    let current_bytes = image.bytes.len() as u64;

    if current_bytes <= target_bytes {
        return Some(image.clone());
    }

    if target_bytes == 0 {
        return None;
    }

    let reader = image::ImageReader::new(Cursor::new(&image.bytes))
        .with_guessed_format()
        .ok()?;

    let decoded = reader.decode().ok()?;

    let (width, height) = (decoded.width(), decoded.height());

    // 根据文件大小比例估算像素缩放比例。
    //
    // 文件大小近似与像素面积相关：
    //
    //   new_area / old_area ≈ target / current
    //
    // 因此单轴缩放比例为：
    //
    //   sqrt(target / current)
    let ratio = ((target_bytes as f64 / current_bytes as f64).sqrt()
        * RESIZE_SAFETY_FACTOR)
        .clamp(MIN_RESIZE_RATIO, 0.95);

    let new_width = ((width as f64 * ratio).round() as u32).max(1);
    let new_height = ((height as f64 * ratio).round() as u32).max(1);

    // 不能继续缩小时停止。
    if new_width >= width && new_height >= height {
        return None;
    }

    let resized = decoded.resize(
        new_width,
        new_height,
        image::imageops::FilterType::Lanczos3,
    );

    let mut output = Vec::new();

    resized
        .write_to(
            &mut Cursor::new(&mut output),
            image::ImageFormat::Jpeg,
        )
        .ok()?;

    // 极端情况下，即使尺寸发生变化，编码结果也可能没有变小。
    // 调用方会进一步处理这种情况，但这里直接拒绝明显无效的结果。
    if output.len() >= image.bytes.len() {
        return None;
    }

    Some(Image::new(
        output,
        Some("image/jpeg".to_string()),
    ))
}

/// 把图片最长边减半并重编码为 JPEG。
///
/// 这是显式的二分降级工具，与 `resize_towards()` 的按目标字节数比例
/// 缩放不同。
///
/// 返回 `None` 表示无法解码、无法编码或图片已经无法继续缩小。
pub fn downscale_image(image: &Image) -> Option<Image> {
    let reader = image::ImageReader::new(Cursor::new(&image.bytes))
        .with_guessed_format()
        .ok()?;

    let decoded = reader.decode().ok()?;

    let (width, height) = (decoded.width(), decoded.height());

    // 已经无法继续有意义地缩小。
    if width <= 2 && height <= 2 {
        return None;
    }

    let new_width = (width / 2).max(1);
    let new_height = (height / 2).max(1);

    // 防止极小图片出现尺寸不变。
    if new_width >= width && new_height >= height {
        return None;
    }

    let resized = decoded.resize(
        new_width,
        new_height,
        image::imageops::FilterType::Lanczos3,
    );

    let mut output = Vec::new();

    resized
        .write_to(
            &mut Cursor::new(&mut output),
            image::ImageFormat::Jpeg,
        )
        .ok()?;

    if output.len() >= image.bytes.len() {
        return None;
    }

    Some(Image::new(
        output,
        Some("image/jpeg".to_string()),
    ))
}

/// 如果图片超过 `limit`，按目标字节数比例逐轮缩放，直到：
///
/// - 图片大小 <= `limit`；或
/// - 达到 `MAX_DOWNSCALE_ROUNDS`；或
/// - 无法继续缩小。
///
/// 注意：如果最终仍然超过 `limit`，函数返回最后一个能够生成的版本，
/// 由调用方决定是跳过、报错还是继续尝试其他 provider/source。
pub fn ensure_within_limit(image: &Image, limit: u64) -> Image {
    if image.bytes.len() as u64 <= limit {
        return image.clone();
    }

    let mut current = image.clone();

    for _ in 0..MAX_DOWNSCALE_ROUNDS {
        let next = match resize_towards(&current, limit) {
            Some(next) => next,
            None => break,
        };

        // 防止异常编码器/输入导致结果没有变小。
        if next.bytes.len() >= current.bytes.len() {
            break;
        }

        current = next;

        if current.bytes.len() as u64 <= limit {
            break;
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

        let mut output = Vec::new();

        img.write_to(
            &mut Cursor::new(&mut output),
            image::ImageFormat::Jpeg,
        )
        .expect("encode jpeg");

        Image::new(
            output,
            Some("image/jpeg".to_string()),
        )
    }

    fn decode_dimensions(image: &Image) -> (u32, u32) {
        let reader = image::ImageReader::new(Cursor::new(&image.bytes))
            .with_guessed_format()
            .expect("guess image format");

        let decoded = reader.decode().expect("decode image");

        (decoded.width(), decoded.height())
    }

    #[test]
    fn downscale_halves_dimensions_and_reencodes_jpeg() {
        let image = make_jpeg(800, 400);

        let downscaled =
            downscale_image(&image).expect("downscale should succeed");

        let (width, height) = decode_dimensions(&downscaled);

        assert_eq!(width, 400);
        assert_eq!(height, 200);
        assert_eq!(
            downscaled.mime_type.as_deref(),
            Some("image/jpeg")
        );
        assert!(
            downscaled.bytes.len() < image.bytes.len(),
            "downscaled image should be smaller"
        );
    }

    #[test]
    fn downscale_tiny_image_returns_none() {
        let image = make_jpeg(2, 2);

        assert!(downscale_image(&image).is_none());
    }

    #[test]
    fn downscale_one_pixel_image_returns_none() {
        let image = make_jpeg(1, 1);

        assert!(downscale_image(&image).is_none());
    }

    #[test]
    fn downscale_invalid_bytes_returns_none() {
        let image = Image::new(
            vec![0u8, 1, 2, 3],
            Some("image/jpeg".to_string()),
        );

        assert!(downscale_image(&image).is_none());
    }

    #[test]
    fn ensure_within_limit_keeps_small_image_unchanged() {
        let image = make_jpeg(64, 64);

        let original_bytes = image.bytes.clone();

        let fitted = ensure_within_limit(
            &image,
            u64::MAX,
        );

        assert_eq!(fitted.bytes, original_bytes);
        assert_eq!(fitted.mime_type, image.mime_type);
    }

    #[test]
    fn ensure_within_limit_downscales_large_image() {
        let image = make_jpeg(2000, 2000);

        let fitted = ensure_within_limit(
            &image,
            200_000,
        );

        assert!(
            fitted.bytes.len() as u64 <= 200_000,
            "fitted size {} should be within limit",
            fitted.bytes.len()
        );
    }

    #[test]
    fn resize_towards_returns_same_when_within_target() {
        let image = make_jpeg(64, 64);

        let output = resize_towards(
            &image,
            1024 * 1024,
        )
        .expect("within target returns clone");

        assert_eq!(output.bytes, image.bytes);
        assert_eq!(output.mime_type, image.mime_type);
    }

    #[test]
    fn resize_towards_scales_proportionally_not_halving() {
        let image = make_jpeg(1200, 1200);

        let target =
            (image.bytes.len() as u64) * 70 / 100;

        let output =
            resize_towards(&image, target)
                .expect("resize should succeed");

        let (width, height) = decode_dimensions(&output);

        // sqrt(0.7) × 0.9 ≈ 0.753
        //
        // 因此 1200px 应该得到约 904px，
        // 而不是机械减半得到 600px。
        assert!(
            width > 700,
            "proportional resize should keep more quality, got width {}",
            width
        );

        assert_eq!(
            width,
            height,
            "square image should remain square"
        );

        assert!(
            output.bytes.len() < image.bytes.len(),
            "resized image should be smaller"
        );

        // 对简单测试图片，0.9 安全系数通常可以直接达到目标。
        //
        // 如果以后更换 JPEG encoder 或 quality 参数，这个断言可能需要
        // 调整为多轮 ensure_within_limit 测试，而不是要求单轮一定成功。
        assert!(
            output.bytes.len() as u64 <= target,
            "resized size {} should be <= target {}",
            output.bytes.len(),
            target
        );
    }

    #[test]
    fn resize_towards_invalid_bytes_returns_none() {
        // target 小于 current 才会触发解码路径；
        // 非法字节解码失败 -> None。
        let image = Image::new(
            vec![0u8, 1, 2, 3],
            Some("image/jpeg".to_string()),
        );

        assert!(resize_towards(&image, 1).is_none());
    }

    #[test]
    fn resize_towards_zero_target_returns_none() {
        let image = make_jpeg(800, 800);

        assert!(resize_towards(&image, 0).is_none());
    }

    #[test]
    fn resize_towards_preserves_aspect_ratio() {
        let image = make_jpeg(1600, 800);

        let output =
            resize_towards(&image, image.bytes.len() as u64 / 2)
                .expect("resize should succeed");

        let (width, height) = decode_dimensions(&output);

        assert_eq!(
            width as f64 / height as f64,
            2.0
        );
    }

    #[test]
    fn resize_towards_changes_mime_to_jpeg() {
        let image = make_jpeg(1200, 1200);

        let output =
            resize_towards(&image, image.bytes.len() as u64 / 2)
                .expect("resize should succeed");

        assert_eq!(
            output.mime_type.as_deref(),
            Some("image/jpeg")
        );
    }

    #[test]
    fn ensure_within_limit_reduces_size() {
        let image = make_jpeg(2000, 2000);

        let original_size = image.bytes.len();

        let fitted =
            ensure_within_limit(&image, original_size as u64 / 4);

        assert!(
            fitted.bytes.len() < original_size,
            "fitted image should be smaller"
        );
    }

    #[test]
    fn ensure_within_limit_returns_original_when_resize_fails() {
        let image = Image::new(
            vec![0u8, 1, 2, 3],
            Some("image/jpeg".to_string()),
        );

        let result =
            ensure_within_limit(&image, 1);

        assert_eq!(result.bytes, image.bytes);
    }
}
