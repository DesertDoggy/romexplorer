//! Media cache tiers and grid thumbnails (docs/plan/database.md, "Media cache").
//!
//! - **Tiers:** the tier 1 / tier 2 cache dirs (Config > Storage) hold copies of media --
//!   full-size copies (`full`) and thumbnails (`thumb:avif`, `thumb:jpeg`, ...) -- tracked
//!   in `media_cache` with a last-access time. A full-size view is served from tier 1, then
//!   tier 2, then the original, copying into tier 1 on a miss. A thumbnail is generated
//!   from the best copy on first request and kept in tier 1 (else tier 2, else the app's
//!   own `_cache` dir, tier 0, never evicted).
//! - **Eviction:** after each insert a tier over its size cap (`media_cache_tier{1,2}_max_mb`,
//!   0 = no cap) drops least-recently-used entries, every full-size copy before any
//!   thumbnail. Only cache copies are removed; originals are never touched.
//! - **Thumbnails:** decoded and resized with the `image` crate, encoded as lossy AVIF
//!   (ravif, pure Rust) or JPEG. WebP and JPEG XL are placeholders that fall back to JPEG
//!   until an encoder is wired in; adding one only fills its `encode_thumb` arm.

use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageEncoder};
use rayon::prelude::*;
use sqlx::mysql::MySqlPool;
use sqlx::Row;

use super::error::DatabaseError;
use super::media_store::{self, Roots};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbFormat {
    Avif,
    Jpeg,
    /// Placeholder: lossy WebP needs libwebp (e.g. the `webp` crate); falls back to JPEG.
    Webp,
    /// Placeholder: JPEG XL needs libjxl (`jpegxl-rs`) or a mature pure-Rust encoder;
    /// falls back to JPEG.
    Jxl,
}

impl ThumbFormat {
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "avif" => Some(Self::Avif),
            "jpeg" | "jpg" => Some(Self::Jpeg),
            "webp" => Some(Self::Webp),
            "jxl" | "jpegxl" => Some(Self::Jxl),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Avif => "avif",
            Self::Jpeg => "jpeg",
            Self::Webp => "webp",
            Self::Jxl => "jxl",
        }
    }

    pub fn ext(self) -> &'static str {
        match self {
            Self::Avif => "avif",
            Self::Jpeg => "jpg",
            Self::Webp => "webp",
            Self::Jxl => "jxl",
        }
    }

    /// The format actually produced for a request of `self` (placeholders -> JPEG).
    pub fn effective(self) -> Self {
        match self {
            Self::Avif | Self::Jpeg => self,
            Self::Webp | Self::Jxl => Self::Jpeg,
        }
    }

    pub fn variant(self) -> String {
        format!("thumb:{}", self.effective().name())
    }
}

/// Resizes so the longest edge is at most `max_px` (never upscales).
pub fn resize_for_thumb(img: &DynamicImage, max_px: u32) -> DynamicImage {
    if img.width() <= max_px && img.height() <= max_px {
        return img.clone();
    }
    img.resize(max_px, max_px, image::imageops::FilterType::Triangle)
}

/// Encodes a thumbnail; returns the bytes and the format actually used.
pub fn encode_thumb(img: &DynamicImage, format: ThumbFormat, quality: u8) -> Result<(Vec<u8>, ThumbFormat), String> {
    let quality = quality.clamp(1, 100);
    let mut out = Vec::new();
    match format {
        ThumbFormat::Avif => {
            let rgba = img.to_rgba8();
            // speed 8 of 1..10: a small quality cost for a much faster encode, fine for
            // grid-sized images.
            image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut out, 8, quality)
                .write_image(rgba.as_raw(), rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8)
                .map_err(|e| format!("avif encode: {e}"))?;
            Ok((out, ThumbFormat::Avif))
        }
        ThumbFormat::Jpeg => {
            let rgb = img.to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality)
                .write_image(rgb.as_raw(), rgb.width(), rgb.height(), image::ExtendedColorType::Rgb8)
                .map_err(|e| format!("jpeg encode: {e}"))?;
            Ok((out, ThumbFormat::Jpeg))
        }
        ThumbFormat::Webp | ThumbFormat::Jxl => {
            log::debug!("thumbnail format {} has no encoder yet; using jpeg", format.name());
            encode_thumb(img, ThumbFormat::Jpeg, quality)
        }
    }
}

/// A client's own copy of an image, as AVIF when that is smaller than what the server sent.
///
/// `None` keeps the original: something that is not a still image this build decodes (a
/// GIF may be animated, SVG is not a bitmap), an image already in AVIF, or one that would not
/// shrink. Never upscaled; an edge over 4096 px is brought down to it, which is more than
/// any screen here shows a cover at. Call from a blocking context.
pub fn optimised_copy(source: &[u8]) -> Option<Vec<u8>> {
    const QUALITY: u8 = 80;
    const MAX_EDGE: u32 = 4096;
    let format = image::guess_format(source).ok()?;
    if !matches!(format, image::ImageFormat::Jpeg | image::ImageFormat::Png | image::ImageFormat::WebP) {
        return None;
    }
    let (bytes, used) = make_thumb(source, MAX_EDGE, ThumbFormat::Avif, QUALITY).ok()?;
    (used == ThumbFormat::Avif && bytes.len() < source.len()).then_some(bytes)
}

/// Decode + resize + encode one source image.
pub fn make_thumb(source: &[u8], max_px: u32, format: ThumbFormat, quality: u8) -> Result<(Vec<u8>, ThumbFormat), String> {
    let img = image::load_from_memory(source).map_err(|e| format!("decode: {e}"))?;
    encode_thumb(&resize_for_thumb(&img, max_px), format, quality)
}

/// `make_thumb` over many sources in parallel (rayon), in input order. Call from a
/// blocking context.
pub fn make_thumbs(sources: &[Vec<u8>], max_px: u32, format: ThumbFormat, quality: u8) -> Vec<Result<(Vec<u8>, ThumbFormat), String>> {
    sources.par_iter().map(|s| make_thumb(s, max_px, format, quality)).collect()
}

/// One `media_cache` row, for eviction planning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheEntry {
    pub media_id: u64,
    pub variant: String,
    pub rel_path: String,
    pub bytes: u64,
    /// Seconds since the epoch.
    pub last_access: i64,
}

/// Which entries to drop so the tier fits `cap` bytes: full-size copies first (least
/// recently used first), thumbnails only once no full-size copy is left.
pub fn eviction_plan(entries: &[CacheEntry], cap: u64) -> Vec<&CacheEntry> {
    let mut total: u64 = entries.iter().map(|e| e.bytes).sum();
    if cap == 0 || total <= cap {
        return Vec::new();
    }
    let mut order: Vec<&CacheEntry> = entries.iter().collect();
    order.sort_by_key(|e| (e.variant.starts_with("thumb"), e.last_access));
    let mut out = Vec::new();
    for e in order {
        if total <= cap {
            break;
        }
        total = total.saturating_sub(e.bytes);
        out.push(e);
    }
    out
}

fn tier_dir(roots: &Roots, tier: u8) -> Option<&Path> {
    match tier {
        0 => Some(roots.tier0.as_path()),
        1 => roots.tier1.as_deref(),
        2 => roots.tier2.as_deref(),
        _ => None,
    }
}

/// `<variant dir>/<id % 256>/<id>.<ext>` -- spread over 256 dirs so none grows huge.
pub fn cache_rel_path(media_id: u64, variant: &str, ext: &str) -> String {
    format!("{}/{:02x}/{media_id}.{ext}", variant.replace(':', "-"), media_id % 256)
}

async fn touch(pool: &MySqlPool, media_id: u64, variant: &str, tier: u8) {
    let _ = sqlx::query("UPDATE media_cache SET last_access = NOW() WHERE media_id = ? AND variant = ? AND tier = ?").bind(media_id).bind(variant).bind(tier).execute(pool).await;
}

/// Cached copies of (media, variant) that still exist on disk, lowest tier first.
async fn cached(pool: &MySqlPool, roots: &Roots, media_id: u64, variant: &str) -> Result<Option<(u8, PathBuf)>, DatabaseError> {
    let rows = sqlx::query("SELECT tier, rel_path FROM media_cache WHERE media_id = ? AND variant = ? ORDER BY (tier = 0), tier").bind(media_id).bind(variant).fetch_all(pool).await?;
    for r in rows {
        let tier: u8 = r.try_get("tier")?;
        let rel: String = r.try_get("rel_path")?;
        let Some(dir) = tier_dir(roots, tier) else { continue };
        let path = media_store::join_rel(dir, &rel);
        if path.is_file() {
            touch(pool, media_id, variant, tier).await;
            return Ok(Some((tier, path)));
        }
        sqlx::query("DELETE FROM media_cache WHERE media_id = ? AND variant = ? AND tier = ?").bind(media_id).bind(variant).bind(tier).execute(pool).await?;
    }
    Ok(None)
}

/// The cached full-size copy of `media_id`, if any tier still has one.
///
/// For when the original is gone: from a client's point of view the original on the server's
/// disk is just the deepest tier, and a shallower tier that still has the bytes is as good a
/// place to serve or restore them from as the provider -- and a great deal cheaper.
pub async fn cached_full(pool: &MySqlPool, roots: &Roots, media_id: u64) -> Result<Option<PathBuf>, DatabaseError> {
    Ok(cached(pool, roots, media_id, "full").await?.map(|(_, path)| path))
}

/// Drops every cached copy of `media_id`, files and rows: after the original was replaced,
/// a cached full-size copy or thumbnail would keep serving the old bytes.
pub async fn forget(pool: &MySqlPool, roots: &Roots, media_id: u64) -> Result<(), DatabaseError> {
    let rows = sqlx::query("SELECT tier, rel_path FROM media_cache WHERE media_id = ?").bind(media_id).fetch_all(pool).await?;
    for r in rows {
        let tier: u8 = r.try_get("tier")?;
        let rel: String = r.try_get("rel_path")?;
        if let Some(dir) = tier_dir(roots, tier) {
            let _ = std::fs::remove_file(media_store::join_rel(dir, &rel));
        }
    }
    sqlx::query("DELETE FROM media_cache WHERE media_id = ?").bind(media_id).execute(pool).await?;
    Ok(())
}

/// Records a cache file and evicts the tier down to its cap.
async fn insert(pool: &MySqlPool, roots: &Roots, caps: (u64, u64), media_id: u64, variant: &str, tier: u8, rel: &str, bytes: u64) -> Result<(), DatabaseError> {
    sqlx::query("INSERT INTO media_cache (media_id, variant, tier, rel_path, bytes) VALUES (?, ?, ?, ?, ?) ON DUPLICATE KEY UPDATE rel_path = VALUES(rel_path), bytes = VALUES(bytes), last_access = NOW()")
        .bind(media_id)
        .bind(variant)
        .bind(tier)
        .bind(rel)
        .bind(bytes)
        .execute(pool)
        .await?;
    let cap = match tier {
        1 => caps.0,
        2 => caps.1,
        _ => 0,
    };
    evict(pool, roots, tier, cap).await
}

/// Drops least-recently-used cache entries of `tier` until it fits `cap` (0 = no cap).
pub async fn evict(pool: &MySqlPool, roots: &Roots, tier: u8, cap: u64) -> Result<(), DatabaseError> {
    let Some(dir) = tier_dir(roots, tier).filter(|_| cap > 0 && tier > 0) else { return Ok(()) };
    let rows = sqlx::query("SELECT media_id, variant, rel_path, bytes, UNIX_TIMESTAMP(last_access) AS ts FROM media_cache WHERE tier = ?").bind(tier).fetch_all(pool).await?;
    let entries: Vec<CacheEntry> = rows
        .iter()
        .map(|r| {
            Ok(CacheEntry {
                media_id: r.try_get("media_id")?,
                variant: r.try_get("variant")?,
                rel_path: r.try_get("rel_path")?,
                bytes: r.try_get("bytes")?,
                last_access: r.try_get::<Option<i64>, _>("ts").ok().flatten().unwrap_or(0),
            })
        })
        .collect::<Result<_, sqlx::Error>>()?;
    for e in eviction_plan(&entries, cap) {
        let path = media_store::join_rel(dir, &e.rel_path);
        if path.starts_with(dir) {
            let _ = tokio::fs::remove_file(&path).await;
        }
        sqlx::query("DELETE FROM media_cache WHERE media_id = ? AND variant = ? AND tier = ?").bind(e.media_id).bind(&e.variant).bind(tier).execute(pool).await?;
    }
    Ok(())
}

/// The file to serve for a full-size view: tier 1, tier 2 (copied up), or the original
/// (copied into tier 1 in the background).
pub async fn serve_full(pool: &MySqlPool, roots: &Roots, caps: (u64, u64), media_id: u64, original: &Path) -> Result<PathBuf, DatabaseError> {
    let hit = cached(pool, roots, media_id, "full").await?;
    if let Some((1, path)) = hit {
        return Ok(path);
    }
    let source = hit.map(|(_, p)| p).unwrap_or_else(|| original.to_path_buf());
    if let Some(tier1) = roots.tier1.clone() {
        let ext = original.extension().and_then(|e| e.to_str()).unwrap_or("bin").to_ascii_lowercase();
        let rel = cache_rel_path(media_id, "full", &ext);
        let (pool, roots, from) = (pool.clone(), roots.clone(), source.clone());
        tokio::spawn(async move {
            let target = media_store::join_rel(&tier1, &rel);
            let copy = async {
                let bytes = tokio::fs::read(&from).await?;
                media_store::write_file(&target, &bytes).await?;
                insert(&pool, &roots, caps, media_id, "full", 1, &rel, bytes.len() as u64).await
            };
            if let Err(e) = copy.await {
                log::debug!("media cache: tier 1 copy of {media_id} failed: {e}");
            }
        });
    }
    Ok(source)
}

/// The thumbnail file for a grid view, generated on first request. `Ok(None)`: the
/// source can't be decoded (e.g. AVIF without `avif-decode`, SVG, video) -- serve the
/// original instead.
pub async fn serve_thumb(pool: &MySqlPool, roots: &Roots, caps: (u64, u64), media_id: u64, original: &Path, max_px: u32, format: ThumbFormat, quality: u8) -> Result<Option<PathBuf>, DatabaseError> {
    let variant = format.variant();
    if let Some((_, path)) = cached(pool, roots, media_id, &variant).await? {
        return Ok(Some(path));
    }
    let source = match cached(pool, roots, media_id, "full").await? {
        Some((_, p)) => p,
        None => original.to_path_buf(),
    };
    let bytes = tokio::fs::read(&source).await?;
    store_thumb(pool, roots, caps, media_id, bytes, max_px, format, quality).await
}

/// Makes the thumbnail of `media_id` from `bytes` and puts it in the cache. `Ok(None)` when
/// the bytes cannot be decoded.
///
/// Its own step because of a client's AVIF copies (`optimised_copy`): this build encodes
/// AVIF but cannot decode it, so a thumbnail has to be made from the bytes the server sent,
/// before they are replaced -- `api::media_sync` calls this at that moment. If that
/// thumbnail is later evicted the grid shows the full-size AVIF instead.
pub async fn store_thumb(pool: &MySqlPool, roots: &Roots, caps: (u64, u64), media_id: u64, bytes: Vec<u8>, max_px: u32, format: ThumbFormat, quality: u8) -> Result<Option<PathBuf>, DatabaseError> {
    let made = tokio::task::spawn_blocking(move || make_thumb(&bytes, max_px, format, quality)).await.map_err(|e| DatabaseError::Server(format!("thumbnail task: {e}")))?;
    let (thumb, used) = match made {
        Ok(v) => v,
        Err(e) => {
            log::debug!("media {media_id}: no thumbnail ({e})");
            return Ok(None);
        }
    };
    let tier: u8 = if roots.tier1.is_some() { 1 } else if roots.tier2.is_some() { 2 } else { 0 };
    let dir = tier_dir(roots, tier).expect("tier dir").to_path_buf();
    let rel = cache_rel_path(media_id, &used.variant(), used.ext());
    let path = media_store::join_rel(&dir, &rel);
    media_store::write_file(&path, &thumb).await?;
    insert(pool, roots, caps, media_id, &used.variant(), tier, &rel, thumb.len() as u64).await?;
    Ok(Some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: u64, variant: &str, bytes: u64, ts: i64) -> CacheEntry {
        CacheEntry { media_id: id, variant: variant.to_string(), rel_path: format!("{id}"), bytes, last_access: ts }
    }

    #[test]
    fn eviction_drops_full_copies_before_thumbs() {
        let entries = vec![entry(1, "thumb:avif", 10, 1), entry(2, "full", 100, 50), entry(3, "full", 100, 10), entry(4, "thumb:jpeg", 10, 2)];
        let ids = |cap| eviction_plan(&entries, cap).iter().map(|e| e.media_id).collect::<Vec<_>>();
        assert_eq!(ids(0), Vec::<u64>::new(), "0 = no cap");
        assert_eq!(ids(1000), Vec::<u64>::new());
        assert_eq!(ids(200), vec![3], "oldest full copy first");
        assert_eq!(ids(20), vec![3, 2], "every full copy before any thumb");
        assert_eq!(ids(5), vec![3, 2, 1, 4], "thumbs last, oldest first");
    }

    #[test]
    fn cache_paths_and_variants() {
        assert_eq!(cache_rel_path(258, "full", "jpg"), "full/02/258.jpg");
        assert_eq!(cache_rel_path(15, "thumb:avif", "avif"), "thumb-avif/0f/15.avif");
        assert_eq!(ThumbFormat::Webp.variant(), "thumb:jpeg");
        assert_eq!(ThumbFormat::Avif.variant(), "thumb:avif");
        assert_eq!(ThumbFormat::from_name("JXL"), Some(ThumbFormat::Jxl));
    }

    fn sample_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(img).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_clients_copy_is_avif_only_when_that_is_smaller_and_a_still_image() {
        // Noise does not compress losslessly, so the PNG is large and AVIF wins.
        let mut seed = 0x2545_F491_4F6C_DD1Du64;
        let noisy = image::RgbImage::from_fn(256, 256, |_, _| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            image::Rgb([seed as u8, (seed >> 8) as u8, (seed >> 16) as u8])
        });
        let mut png = std::io::Cursor::new(Vec::new());
        DynamicImage::ImageRgb8(noisy).write_to(&mut png, image::ImageFormat::Png).unwrap();
        let png = png.into_inner();
        let avif = optimised_copy(&png).expect("a noisy PNG shrinks");
        assert!(avif.len() < png.len());
        assert_eq!(&avif[4..12], b"ftypavif");
        // Already AVIF, not an image at all, and a format that may be animated: left alone.
        assert!(optimised_copy(&avif).is_none());
        assert!(optimised_copy(b"not an image").is_none());
        assert!(optimised_copy(b"GIF89a\x01\x00\x01\x00\x00\x00\x00;").is_none());
    }

    #[test]
    fn thumbnails_resize_and_encode() {
        let png = sample_png(640, 480);
        let (jpeg, used) = make_thumb(&png, 320, ThumbFormat::Jpeg, 70).unwrap();
        assert_eq!(used, ThumbFormat::Jpeg);
        let back = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((back.width(), back.height()), (320, 240));
        let (avif, used) = make_thumb(&png, 320, ThumbFormat::Avif, 60).unwrap();
        assert_eq!(used, ThumbFormat::Avif);
        assert_eq!(&avif[4..12], b"ftypavif", "an AVIF container");
        let small = sample_png(100, 50);
        let (jpeg, _) = make_thumb(&small, 320, ThumbFormat::Jpeg, 70).unwrap();
        assert_eq!(image::load_from_memory(&jpeg).unwrap().width(), 100, "never upscaled");
    }

    #[test]
    fn placeholder_formats_fall_back_to_jpeg() {
        let png = sample_png(64, 64);
        for f in [ThumbFormat::Webp, ThumbFormat::Jxl] {
            let (bytes, used) = make_thumb(&png, 32, f, 60).unwrap();
            assert_eq!(used, ThumbFormat::Jpeg);
            assert_eq!(&bytes[..2], &[0xFF, 0xD8]);
        }
    }

    #[test]
    fn parallel_batch_matches_sequential() {
        let sources: Vec<Vec<u8>> = (1..=6).map(|i| sample_png(100 + i * 37, 80 + i * 11)).collect();
        let parallel = make_thumbs(&sources, 64, ThumbFormat::Jpeg, 75);
        let sequential: Vec<_> = sources.iter().map(|s| make_thumb(s, 64, ThumbFormat::Jpeg, 75)).collect();
        assert_eq!(parallel, sequential);
        assert!(make_thumb(b"not an image", 64, ThumbFormat::Jpeg, 75).is_err());
    }
}
