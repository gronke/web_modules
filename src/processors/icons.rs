//! Generate favicons and app icons from a source PNG, via [`image`] + [`ico`].
//!
//! Pure-Rust raster scaling, no ImageMagick/sharp. Source images are decoded as
//! PNG (the only `image` codec enabled).

use std::path::{Path, PathBuf};

use image::imageops::FilterType;
use image::{ImageReader, Limits};

use crate::{Error, Result};

/// Decode cap for a source image, in pixels per side. An icon source has no business being
/// larger; the strict dimension limit makes the PNG decoder refuse at the header, before
/// allocating for the declared size, so a crafted PNG cannot exhaust memory.
const MAX_SOURCE_DIMENSION: u32 = 4096;

/// Decode a source PNG with [`MAX_SOURCE_DIMENSION`] enforced (and the `image` crate's
/// default 512 MiB allocation cap kept).
fn open_source(src: &Path) -> std::result::Result<image::DynamicImage, Box<dyn std::error::Error>> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIMENSION);
    limits.max_image_height = Some(MAX_SOURCE_DIMENSION);
    let mut reader = ImageReader::open(src)?.with_guessed_format()?;
    reader.limits(limits);
    Ok(reader.decode()?)
}

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
    let img = open_source(src)?;
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
    let resized = open_source(src)?.resize_exact(size, size, FilterType::Lanczos3);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    resized.save(out)?;
    Ok(())
}

/// A standard icon set generated from one source image: a multi-resolution `favicon.ico`,
/// an apple-touch-icon, and a PNG per PWA-manifest size. Created with [`from_image_path`]
/// plus a few chainable overrides, then [`IconOptions::generate`]d.
///
/// A configurable source + destination layer over the [`favicon`]/[`png`] primitives that
/// also emits the matching `<link>` tags and the manifest icon list, composed once, then
/// rendered, like an import map.
#[derive(Clone, Debug)]
pub struct IconOptions {
    src: PathBuf,
    out_dir: PathBuf,
    url_prefix: String,
    favicon_name: String,
    icons_dir: String,
    favicon_sizes: Vec<u32>,
    apple_touch_size: u32,
    manifest_sizes: Vec<u32>,
}

/// Start an icon-set build from a source image (PNG).
///
/// Defaults: written into `.`; favicon served at `/favicon.ico`; the other icons under
/// `/images/icons/`; favicon resolutions `16/32/48`; apple-touch `180`; PWA-manifest icons
/// `192` and `512`. Every default is overridable below.
///
/// ```no_run
/// let icons = web_modules::icons::from_image_path("logo.png")
///     .out_dir("dist")
///     .generate()
///     .unwrap();
/// assert!(icons.to_link_tags().contains("apple-touch-icon"));
/// ```
pub fn from_image_path(src: impl Into<PathBuf>) -> IconOptions {
    IconOptions {
        src: src.into(),
        out_dir: PathBuf::from("."),
        url_prefix: "/".to_string(),
        favicon_name: "favicon.ico".to_string(),
        icons_dir: "images/icons".to_string(),
        favicon_sizes: vec![16, 32, 48],
        apple_touch_size: 180,
        manifest_sizes: vec![192, 512],
    }
}

impl IconOptions {
    /// Directory the generated files are written under (default `.`).
    pub fn out_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.out_dir = dir.into();
        self
    }

    /// URL prefix the generated icons are served at (default `/`).
    pub fn url_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.url_prefix = prefix.into();
        self
    }

    /// File name (relative to the output root) for the favicon (default `favicon.ico`).
    pub fn favicon_name(mut self, name: impl Into<String>) -> Self {
        self.favicon_name = name.into();
        self
    }

    /// Subdirectory (relative to the output root) for the non-favicon icons
    /// (default `images/icons`).
    pub fn icons_dir(mut self, dir: impl Into<String>) -> Self {
        self.icons_dir = dir.into();
        self
    }

    /// `favicon.ico` resolutions (default `[16, 32, 48]`).
    pub fn favicon_sizes(mut self, sizes: impl Into<Vec<u32>>) -> Self {
        self.favicon_sizes = sizes.into();
        self
    }

    /// apple-touch-icon size (default `180`).
    pub fn apple_touch_size(mut self, size: u32) -> Self {
        self.apple_touch_size = size;
        self
    }

    /// PWA-manifest icon sizes (default `[192, 512]`).
    pub fn manifest_sizes(mut self, sizes: impl Into<Vec<u32>>) -> Self {
        self.manifest_sizes = sizes.into();
        self
    }

    /// Generate the set: write `favicon.ico`, `apple-touch-icon.png`, and one PNG per
    /// manifest size, then return the [`Icons`] describing the served URLs.
    pub fn generate(&self) -> Result<Icons> {
        favicon(
            &self.src,
            &self.out_dir.join(&self.favicon_name),
            &self.favicon_sizes,
        )?;

        let icons_root = self.out_dir.join(&self.icons_dir);

        let apple_touch_name = "apple-touch-icon.png";
        png(
            &self.src,
            &icons_root.join(apple_touch_name),
            self.apple_touch_size,
        )?;

        let mut manifest_icons = Vec::with_capacity(self.manifest_sizes.len());
        for &size in &self.manifest_sizes {
            let name = format!("icon-{size}.png");
            png(&self.src, &icons_root.join(&name), size)?;
            manifest_icons.push(IconRef {
                href: self.href(&format!("{}/{name}", self.icons_dir)),
                size,
            });
        }

        Ok(Icons {
            favicon: self.href(&self.favicon_name),
            apple_touch: self.href(&format!("{}/{apple_touch_name}", self.icons_dir)),
            manifest_icons,
        })
    }

    /// Join the configured URL prefix with a relative (forward-slash) path.
    fn href(&self, rel: &str) -> String {
        format!(
            "{}/{}",
            self.url_prefix.trim_end_matches('/'),
            rel.trim_start_matches('/')
        )
    }
}

/// One generated PWA-manifest icon: the size it was rendered at and the URL it's served
/// from. Consumed by the (forthcoming) `manifest` feature to populate `icons[]`.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct IconRef {
    /// Served URL, e.g. `/images/icons/icon-192.png`.
    pub href: String,
    /// Square pixel size.
    pub size: u32,
}

/// The result of [`IconOptions::generate`]: the served URLs of the generated icons, plus
/// helpers to wire them into HTML (and, later, a web app manifest).
#[derive(Clone, Debug)]
pub struct Icons {
    favicon: String,
    apple_touch: String,
    manifest_icons: Vec<IconRef>,
}

impl Icons {
    /// The `<link>` tags for the favicon and apple-touch icon, ready to inject into an HTML
    /// `<head>` (the manifest-size icons are referenced from the web app manifest instead).
    /// Mirrors the import map's `to_script_tag`.
    pub fn to_link_tags(&self) -> String {
        format!(
            "<link rel=\"icon\" href=\"{}\" sizes=\"any\">\n\
             <link rel=\"apple-touch-icon\" href=\"{}\">",
            self.favicon, self.apple_touch
        )
    }

    /// The favicon URL (`/favicon.ico` by default).
    pub fn favicon(&self) -> &str {
        &self.favicon
    }

    /// The apple-touch-icon URL.
    pub fn apple_touch(&self) -> &str {
        &self.apple_touch
    }

    /// The generated PWA-manifest icons (size + URL).
    pub fn manifest_icons(&self) -> &[IconRef] {
        &self.manifest_icons
    }
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

    /// CRC-32 (IEEE), for hand-crafting a valid PNG header in the bomb test.
    fn crc32(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &byte in data {
            crc ^= byte as u32;
            for _ in 0..8 {
                crc = (crc >> 1) ^ (0xEDB8_8320 & (0u32.wrapping_sub(crc & 1)));
            }
        }
        !crc
    }

    /// A PNG whose IHDR declares `w`×`h` (8-bit RGBA), with no image data behind it.
    fn png_header(w: u32, h: u32) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = b"IHDR".to_vec();
        ihdr.extend(w.to_be_bytes());
        ihdr.extend(h.to_be_bytes());
        ihdr.extend([8, 6, 0, 0, 0]);
        let crc = crc32(&ihdr);
        png.extend(13u32.to_be_bytes());
        png.extend(&ihdr);
        png.extend(crc.to_be_bytes());
        png
    }

    #[test]
    fn a_source_declaring_absurd_dimensions_is_refused_before_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("bomb.png");
        // 100000×100000 RGBA would allocate ~40 GB if the decoder trusted the header.
        std::fs::write(&src, png_header(100_000, 100_000)).unwrap();
        let err = png(&src, &dir.path().join("out.png"), 180).unwrap_err();
        assert!(
            err.to_string().contains("limit") || err.to_string().contains("large"),
            "{err}"
        );
        // A header inside the cap still decodes (and errors only on the missing data).
        let src = dir.path().join("small.png");
        std::fs::write(&src, png_header(8, 8)).unwrap();
        let err = png(&src, &dir.path().join("out.png"), 180).unwrap_err();
        assert!(
            !err.to_string().contains("limit") && !err.to_string().contains("large"),
            "{err}"
        );
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

    #[test]
    fn generate_writes_full_set_and_link_tags() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("logo.png");
        write_src_png(&src);
        let out = dir.path().join("dist");

        let icons = from_image_path(&src).out_dir(&out).generate().unwrap();

        // Files on disk.
        assert!(out.join("favicon.ico").exists());
        assert!(out.join("images/icons/apple-touch-icon.png").exists());
        assert!(out.join("images/icons/icon-192.png").exists());
        assert!(out.join("images/icons/icon-512.png").exists());

        // Served URLs + link tags.
        assert_eq!(icons.favicon(), "/favicon.ico");
        assert_eq!(icons.apple_touch(), "/images/icons/apple-touch-icon.png");
        let tags = icons.to_link_tags();
        assert!(tags.contains("rel=\"icon\" href=\"/favicon.ico\""));
        assert!(
            tags.contains("rel=\"apple-touch-icon\" href=\"/images/icons/apple-touch-icon.png\"")
        );

        // Manifest icons exposed (for the manifest feature), in declared order.
        let sizes: Vec<u32> = icons.manifest_icons().iter().map(|i| i.size).collect();
        assert_eq!(sizes, vec![192, 512]);
        assert_eq!(icons.manifest_icons()[0].href, "/images/icons/icon-192.png");
    }

    #[test]
    fn url_prefix_and_overrides_apply() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("logo.png");
        write_src_png(&src);
        let out = dir.path().join("d");

        let icons = from_image_path(&src)
            .out_dir(&out)
            .url_prefix("/app/")
            .manifest_sizes(vec![192])
            .generate()
            .unwrap();

        assert_eq!(icons.favicon(), "/app/favicon.ico");
        assert_eq!(
            icons.apple_touch(),
            "/app/images/icons/apple-touch-icon.png"
        );
        assert_eq!(icons.manifest_icons().len(), 1);
        assert_eq!(
            icons.manifest_icons()[0].href,
            "/app/images/icons/icon-192.png"
        );
    }
}
