//! The format pipeline: which formats this build reads and writes, and what one picture needs
//! to become one an emulator reads. A format is one row of [`Codec`]: adding one (lossy WebP,
//! once the encoder exists; AVIF reading, with `avif-decode`) is a row here and a checkbox in
//! the emulator dialog, nothing else.

use std::io::Cursor;

use image::{DynamicImage, ImageFormat};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    Png,
    Jpg,
    Webp,
    Gif,
    Avif,
}

/// JPEG quality for a converted picture: a box scan, so near-lossless rather than small.
const JPEG_QUALITY: u8 = 90;

impl Codec {
    pub const ALL: [Codec; 5] = [Codec::Png, Codec::Jpg, Codec::Webp, Codec::Gif, Codec::Avif];

    pub fn name(self) -> &'static str {
        match self {
            Codec::Png => "png",
            Codec::Jpg => "jpg",
            Codec::Webp => "webp",
            Codec::Gif => "gif",
            Codec::Avif => "avif",
        }
    }

    /// The file extension it is written with.
    pub fn ext(self) -> &'static str {
        self.name()
    }

    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "png" => Some(Codec::Png),
            "jpg" | "jpeg" => Some(Codec::Jpg),
            "webp" => Some(Codec::Webp),
            "gif" => Some(Codec::Gif),
            "avif" => Some(Codec::Avif),
            _ => None,
        }
    }

    /// What a file's bytes are.
    pub fn sniff(bytes: &[u8]) -> Option<Self> {
        match image::guess_format(bytes).ok()? {
            ImageFormat::Png => Some(Codec::Png),
            ImageFormat::Jpeg => Some(Codec::Jpg),
            ImageFormat::WebP => Some(Codec::Webp),
            ImageFormat::Gif => Some(Codec::Gif),
            ImageFormat::Avif => Some(Codec::Avif),
            _ => None,
        }
    }

    /// Whether this build can decode it. AVIF needs the C decoder (`avif-decode`), which
    /// default builds leave out.
    pub fn can_read(self) -> bool {
        match self {
            Codec::Avif => cfg!(feature = "avif-decode"),
            _ => true,
        }
    }

    /// Whether this build can encode it. WebP's encoder here is lossless only -- a box scan
    /// would come out bigger than the PNG -- so it is not offered until a lossy one is.
    pub fn can_write(self) -> bool {
        matches!(self, Codec::Png | Codec::Jpg | Codec::Avif)
    }
}

/// What one picture needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Already a format the emulator reads, and narrow enough: the bytes as they are.
    Copy(Codec),
    /// Decoded, scaled down to the width if wider, and written as this.
    Convert { to: Codec, width: Option<u32> },
}

/// What a picture in `source` format, `width` pixels wide, needs to become one of `accepted`
/// (in order of preference) at most `max_width` wide. None: no accepted format can be written
/// and the source isn't one of them either.
pub fn plan(source: Codec, width: u32, accepted: &[Codec], max_width: Option<u32>) -> Option<Step> {
    let too_wide = max_width.is_some_and(|m| width > m);
    if accepted.contains(&source) && !too_wide {
        return Some(Step::Copy(source));
    }
    let to = accepted.iter().copied().find(|c| c.can_write())?;
    if !source.can_read() {
        return None;
    }
    Some(Step::Convert { to, width: max_width.filter(|m| width > *m) })
}

/// The width of a picture, read from its header.
pub fn width_of(bytes: &[u8]) -> Option<u32> {
    image::ImageReader::new(Cursor::new(bytes)).with_guessed_format().ok()?.into_dimensions().ok().map(|(w, _)| w)
}

/// Decodes, scales down to `width` if given (keeping the aspect, never up), and encodes as
/// `to`. PNG keeps transparency (logos); JPEG has none, so it is flattened.
pub fn convert(bytes: &[u8], to: Codec, width: Option<u32>) -> Result<Vec<u8>, String> {
    let img = image::load_from_memory(bytes).map_err(|e| format!("can't read the picture: {e}"))?;
    let img = match width {
        Some(w) if w < img.width() => {
            let h = ((img.height() as u64 * w as u64) / img.width().max(1) as u64).max(1) as u32;
            img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        }
        _ => img,
    };
    encode(&img, to)
}

fn encode(img: &DynamicImage, to: Codec) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    match to {
        Codec::Png => img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png).map_err(|e| e.to_string())?,
        Codec::Jpg => {
            let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
            let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, JPEG_QUALITY);
            encoder.encode_image(&rgb).map_err(|e| e.to_string())?;
        }
        Codec::Avif => {
            let encoder = image::codecs::avif::AvifEncoder::new_with_speed_quality(&mut out, 6, 80);
            img.to_rgba8().write_with_encoder(encoder).map_err(|e| e.to_string())?;
        }
        other => return Err(format!("this build can't write {}", other.name())),
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(w: u32, h: u32, format: ImageFormat) -> Vec<u8> {
        let img = DynamicImage::ImageRgba8(image::RgbaImage::from_fn(w, h, |x, y| image::Rgba([(x % 256) as u8, (y % 256) as u8, 128, if x < 2 { 0 } else { 255 }])));
        let mut out = Vec::new();
        let img = if format == ImageFormat::Jpeg { DynamicImage::ImageRgb8(img.to_rgb8()) } else { img };
        img.write_to(&mut Cursor::new(&mut out), format).unwrap();
        out
    }

    #[test]
    fn an_accepted_picture_is_copied_as_it_is() {
        assert_eq!(plan(Codec::Png, 400, &[Codec::Png], Some(512)), Some(Step::Copy(Codec::Png)));
        assert_eq!(plan(Codec::Jpg, 400, &[Codec::Jpg, Codec::Png], None), Some(Step::Copy(Codec::Jpg)), "JPG kept where it is read");
    }

    #[test]
    fn anything_else_is_converted_to_the_first_writable_format() {
        assert_eq!(plan(Codec::Webp, 400, &[Codec::Png], Some(512)), Some(Step::Convert { to: Codec::Png, width: None }));
        assert_eq!(plan(Codec::Jpg, 400, &[Codec::Png], Some(512)), Some(Step::Convert { to: Codec::Png, width: None }), "RetroArch without non-PNG");
        // WebP first in the list, but not writable here: the next one.
        assert_eq!(plan(Codec::Gif, 400, &[Codec::Webp, Codec::Jpg], None), Some(Step::Convert { to: Codec::Jpg, width: None }));
        assert_eq!(plan(Codec::Gif, 400, &[Codec::Webp], None), None, "nothing it can write");
    }

    #[test]
    fn a_wide_picture_is_scaled_down_never_up() {
        assert_eq!(plan(Codec::Png, 1000, &[Codec::Png], Some(512)), Some(Step::Convert { to: Codec::Png, width: Some(512) }));
        assert_eq!(plan(Codec::Png, 300, &[Codec::Png], Some(512)), Some(Step::Copy(Codec::Png)));
        let out = convert(&picture(1000, 1400, ImageFormat::Png), Codec::Png, Some(512)).unwrap();
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (512, 716));
        let small = convert(&picture(100, 140, ImageFormat::Png), Codec::Png, Some(512)).unwrap();
        assert_eq!(image::load_from_memory(&small).unwrap().width(), 100);
    }

    #[test]
    fn png_keeps_transparency_and_formats_round_trip() {
        let out = convert(&picture(40, 40, ImageFormat::Png), Codec::Png, None).unwrap();
        assert_eq!(Codec::sniff(&out), Some(Codec::Png));
        let img = image::load_from_memory(&out).unwrap().to_rgba8();
        assert_eq!(img.get_pixel(0, 0)[3], 0, "the transparent column stays transparent");
        let jpg = convert(&picture(40, 40, ImageFormat::Png), Codec::Jpg, None).unwrap();
        assert_eq!(Codec::sniff(&jpg), Some(Codec::Jpg));
        let from_webp = convert(&picture(40, 40, ImageFormat::WebP), Codec::Png, None).unwrap();
        assert_eq!(Codec::sniff(&from_webp), Some(Codec::Png));
        assert_eq!(width_of(&jpg), Some(40));
    }
}
