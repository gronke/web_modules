//! Generate favicons and app icons from a source PNG, via [`image`] + [`ico`].
//!
//! Pure-Rust raster scaling — no ImageMagick/sharp. Source images are decoded as
//! PNG (the only `image` codec enabled).

use std::path::Path;

use image::imageops::FilterType;

use crate::{Error, Result};

/// Write a multi-resolution `favicon.ico` (one entry per size) scaled from `src`.
/// Sizes are square pixel dimensions, e.g. `&[16, 32, 48]`.
pub fn favicon(src: &Path, out: &Path, sizes: &[u32]) -> Result<()> {
    favicon_inner(src, out, sizes).map_err(|e| Error::Icons(e.to_string()))
}

fn favicon_inner(
    src: &Path,
    out: &Path,
    sizes: &[u32],
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let img = image::open(src)?;
    let mut dir = ico::IconDir::new(ico::ResourceType::Icon);
    for &size in sizes {
        let rgba = img
            .resize_exact(size, size, FilterType::Lanczos3)
            .to_rgba8();
        let icon = ico::IconImage::from_rgba_data(size, size, rgba.into_raw());
        dir.add_entry(ico::IconDirEntry::encode(&icon)?);
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    dir.write(std::fs::File::create(out)?)?;
    Ok(())
}

/// Write a square `size`×`size` PNG (e.g. an apple-touch-icon) scaled from `src`.
pub fn png(src: &Path, out: &Path, size: u32) -> Result<()> {
    png_inner(src, out, size).map_err(|e| Error::Icons(e.to_string()))
}

fn png_inner(
    src: &Path,
    out: &Path,
    size: u32,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let resized = image::open(src)?.resize_exact(size, size, FilterType::Lanczos3);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    resized.save(out)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_src_png(path: &Path) {
        let mut img = image::RgbaImage::new(64, 64);
        for (x, y, px) in img.enumerate_pixels_mut() {
            *px = image::Rgba([(x * 4) as u8, (y * 4) as u8, 128, 255]);
        }
        img.save(path).unwrap();
    }

    #[test]
    fn favicon_and_png() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("logo.png");
        write_src_png(&src);

        let ico = dir.path().join("favicon.ico");
        favicon(&src, &ico, &[16, 32]).unwrap();
        let bytes = std::fs::read(&ico).unwrap();
        assert!(
            bytes.len() > 4 && bytes[0..4] == [0, 0, 1, 0],
            "ICO magic header"
        );

        let touch = dir.path().join("apple-touch-icon.png");
        png(&src, &touch, 180).unwrap();
        assert!(touch.metadata().unwrap().len() > 0);
    }
}
