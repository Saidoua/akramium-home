//! Thumbnails for images: decoded once, cached by content hash under `data_dir/cache/thumbs`.

use super::Store;
use home_core::{Error, Result};
use std::path::{Path, PathBuf};

pub const EDGE: u32 = 256;

pub fn supported(mime: &str) -> bool {
    matches!(mime, "image/jpeg" | "image/png" | "image/webp" | "image/gif")
}

fn render(source: &Path, target: &Path) -> Result<()> {
    use image::ImageDecoder;
    let bad = |e: image::ImageError| Error::BadRequest(format!("that image cannot be read: {e}"));

    let mut reader = image::ImageReader::open(source)?.with_guessed_format()?;
    // A small file can claim an enormous canvas; refuse before allocating it.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(512 << 20);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(bad)?;
    let orientation = decoder.orientation().map_err(bad)?;
    let mut img = image::DynamicImage::from_decoder(decoder).map_err(bad)?;
    img.apply_orientation(orientation);
    // JPEG has no transparency: lay the picture on white, or clear areas turn black.
    let rgba = img.thumbnail(EDGE, EDGE).to_rgba8();
    let mut small = image::RgbImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let a = p[3] as u32;
        let over = |c: u8| ((c as u32 * a + 255 * (255 - a)) / 255) as u8;
        small.put_pixel(x, y, image::Rgb([over(p[0]), over(p[1]), over(p[2])]));
    }

    let tmp = target.with_extension("tmp");
    {
        let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 82);
        small.write_with_encoder(encoder).map_err(bad)?;
    }
    std::fs::rename(&tmp, target)?;
    Ok(())
}

impl Store {
    fn thumbs_dir(&self) -> Result<PathBuf> {
        let base = self.root().parent().map(Path::to_path_buf).unwrap_or_else(|| self.root().to_path_buf());
        let dir = base.join("cache").join("thumbs");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    /// The path of a JPEG thumbnail for a live image entry, rendering it on first use.
    pub async fn thumbnail(&self, user_id: i64, id: i64) -> Result<PathBuf> {
        let entry = self.entry(user_id, id).await?;
        let mime = entry.mime.as_deref().unwrap_or("");
        if entry.is_dir || !supported(mime) {
            return Err(Error::NotFound);
        }
        let hash = entry.hash.clone().ok_or(Error::NotFound)?;
        if !hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Internal("unexpected hash format".into()));
        }
        let target = self.thumbs_dir()?.join(format!("{hash}.jpg"));
        if target.exists() {
            return Ok(target);
        }
        let source = self.path_of(user_id, id).await?;
        let out = target.clone();
        tokio::task::spawn_blocking(move || render(&source, &out)).await??;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use home_core::{Db, accounts};

    #[tokio::test]
    async fn renders_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open_memory().await.unwrap();
        db.migrate("home-core", home_core::MIGRATIONS).await.unwrap();
        db.migrate("dekave", crate::MIGRATIONS).await.unwrap();
        let uid = accounts::create(&db, "alice", "password1", true).await.unwrap().id;
        let store = Store::new(db, dir.path().join("users"));

        let mut png = Vec::new();
        image::RgbImage::from_pixel(1000, 400, image::Rgb([63, 107, 74]))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let body = futures_util::stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from(png))]);
        let entry = store.create_file(uid, None, "wide.png", body).await.unwrap();

        let thumb = store.thumbnail(uid, entry.id).await.unwrap();
        let small = image::open(&thumb).unwrap();
        assert_eq!((small.width(), small.height()), (256, 102));
        assert!(thumb.starts_with(dir.path().join("cache/thumbs")));
        let again = store.thumbnail(uid, entry.id).await.unwrap();
        assert_eq!(thumb, again);

        let mut clear = Vec::new();
        image::RgbaImage::from_pixel(64, 64, image::Rgba([0, 0, 0, 0]))
            .write_to(&mut std::io::Cursor::new(&mut clear), image::ImageFormat::Png)
            .unwrap();
        let body = futures_util::stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from(clear))]);
        let glass = store.create_file(uid, None, "glass.png", body).await.unwrap();
        let t = image::open(store.thumbnail(uid, glass.id).await.unwrap()).unwrap().to_rgb8();
        assert!(t.get_pixel(5, 5).0.iter().all(|&c| c > 245), "transparent areas come out white, not black");

        let text = futures_util::stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"not an image"))]);
        let note = store.create_file(uid, None, "note.txt", text).await.unwrap();
        assert!(matches!(store.thumbnail(uid, note.id).await, Err(Error::NotFound)));
        let fake = futures_util::stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"not an image"))]);
        let liar = store.create_file(uid, None, "liar.png", fake).await.unwrap();
        assert!(matches!(store.thumbnail(uid, liar.id).await, Err(Error::BadRequest(_) | Error::Io(_))));
    }
}
