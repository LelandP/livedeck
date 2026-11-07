//! ImageCache against wiremock: hits skip the network, the cap holds, junk is never cached and
//! a corrupt cache file is refetched.

use std::fs;
use std::io::Cursor;
use std::path::Path;

use htpc_storage::{Error, ImageCache, Paths};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A `w x h` PNG in a single colour. Noise-free PNGs compress, so sizes stay small.
fn png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(w, h, image::Rgba(rgba));
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// The dev box sets HTTP(S)_PROXY, which cannot reach the local mock server.
fn client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn open(dir: &Path, max_bytes: u64) -> ImageCache {
    ImageCache::with_client(&Paths::under(dir).image_cache_dir(), max_bytes, client()).unwrap()
}

async fn serve(server: &MockServer, at: &str, body: Vec<u8>, times: u64) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .expect(times)
        .mount(server)
        .await;
}

fn cache_files(dir: &Path) -> Vec<std::path::PathBuf> {
    fs::read_dir(Paths::under(dir).image_cache_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect()
}

#[tokio::test]
async fn second_get_is_served_from_disk_even_after_a_restart() {
    let server = MockServer::start().await;
    serve(&server, "/a.png", png(4, 3, [255, 0, 0, 255]), 1).await;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("{}/a.png", server.uri());

    let img = open(dir.path(), 1 << 20).get(&url).await.unwrap();
    assert_eq!((img.width, img.height), (4, 3));
    assert_eq!(img.rgba.len(), 4 * 3 * 4);
    assert_eq!(&img.rgba[..4], &[255, 0, 0, 255]);

    // A fresh cache on the same folder (next app start) must not hit the server again.
    let cache = open(dir.path(), 1 << 20);
    assert_eq!(cache.stats().entries, 1);
    assert_eq!(cache.get(&url).await.unwrap(), img);
    cache.prefetch(&url).await.unwrap();
    // `expect(1)` is verified when the server drops.
}

#[tokio::test]
async fn cache_stays_under_its_cap() {
    let server = MockServer::start().await;
    let body = png(64, 64, [0, 0, 255, 255]);
    let size = body.len() as u64;
    for i in 0..6 {
        serve(&server, &format!("/{i}.png"), body.clone(), 1).await;
    }
    let dir = tempfile::tempdir().unwrap();
    let cap = size * 3 + size / 2;
    let cache = open(dir.path(), cap);

    for i in 0..6 {
        cache
            .prefetch(&format!("{}/{i}.png", server.uri()))
            .await
            .unwrap();
        let stats = cache.stats();
        assert!(stats.total_bytes <= cap, "{stats:?}");
    }
    assert_eq!(cache.stats().entries, 3);
    let on_disk: u64 = cache_files(dir.path())
        .iter()
        .map(|p| fs::metadata(p).unwrap().len())
        .sum();
    assert!(on_disk <= cap, "disk usage {on_disk} > cap {cap}");

    // A restart with a smaller cap evicts down to it.
    let smaller = open(dir.path(), size);
    assert_eq!(smaller.stats().entries, 1);
    assert_eq!(cache_files(dir.path()).len(), 1);
}

#[tokio::test]
async fn undecodable_downloads_are_rejected_and_not_cached() {
    let server = MockServer::start().await;
    serve(
        &server,
        "/junk.png",
        b"<html>not an image</html>".to_vec(),
        1,
    )
    .await;
    let dir = tempfile::tempdir().unwrap();
    let cache = open(dir.path(), 1 << 20);

    let err = cache
        .get(&format!("{}/junk.png", server.uri()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Decode(_)), "{err}");
    assert_eq!(cache.stats().entries, 0);
    assert!(cache_files(dir.path()).is_empty());
}

#[tokio::test]
async fn http_errors_are_reported() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&server)
        .await;
    let dir = tempfile::tempdir().unwrap();
    let err = open(dir.path(), 1 << 20)
        .get(&format!("{}/missing.png", server.uri()))
        .await
        .unwrap_err();
    assert!(matches!(err, Error::HttpStatus(404)), "{err}");
}

#[tokio::test]
async fn corrupt_cache_file_is_refetched() {
    let server = MockServer::start().await;
    serve(&server, "/c.png", png(2, 2, [0, 255, 0, 255]), 2).await;
    let dir = tempfile::tempdir().unwrap();
    let url = format!("{}/c.png", server.uri());
    let cache = open(dir.path(), 1 << 20);
    cache.get(&url).await.unwrap();

    // Damage the cached file behind the cache's back.
    let files = cache_files(dir.path());
    assert_eq!(files.len(), 1);
    fs::write(&files[0], b"bitrot").unwrap();

    let img = cache.get(&url).await.unwrap();
    assert_eq!((img.width, img.height), (2, 2));
    assert_eq!(cache.stats().entries, 1);
    assert_ne!(fs::read(&files[0]).unwrap(), b"bitrot");
}

#[test]
fn big_pictures_are_scaled_down_to_card_size_and_small_ones_kept() {
    let big = htpc_storage::decode(&png(1920, 1080, [0, 0, 255, 255])).unwrap();
    assert_eq!(
        (big.width, big.height),
        (htpc_storage::MAX_DECODED_SIDE, 360)
    );
    assert_eq!(big.rgba.len(), (640 * 360 * 4) as usize);
    let tall = htpc_storage::decode(&png(300, 1200, [0, 0, 255, 255])).unwrap();
    assert_eq!(tall.height, htpc_storage::MAX_DECODED_SIDE);
    let card = htpc_storage::decode(&png(440, 248, [0, 0, 255, 255])).unwrap();
    assert_eq!((card.width, card.height), (440, 248));
}
