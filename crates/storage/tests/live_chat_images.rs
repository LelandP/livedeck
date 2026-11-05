//! The image cache against Twitch's real CDN, for the kinds of image chat shows. Ignored by
//! default (needs the network): `cargo test -p htpc-storage --test live_chat_images -- --ignored`.

use htpc_storage::ImageCache;

#[tokio::test]
#[ignore = "needs the network"]
async fn live_emote_and_badge_images_decode() {
    let dir = tempfile::tempdir().unwrap();
    let cache = ImageCache::open(dir.path(), 8 * 1024 * 1024).unwrap();
    let urls = [
        // Kappa (emote 25), the static image the chat client asks for.
        "https://static-cdn.jtvnw.net/emoticons/v2/25/static/dark/2.0",
        // The global moderator badge, 2x.
        "https://static-cdn.jtvnw.net/badges/v1/3267646d-33f0-4b17-b3df-f923a41db1d0/2",
    ];
    for url in urls {
        let image = cache
            .get(url)
            .await
            .unwrap_or_else(|e| panic!("{url}: {e}"));
        println!("{url}: {}x{}", image.width, image.height);
        assert!(image.width > 0 && image.height > 0);
        assert_eq!(
            image.rgba.len(),
            image.width as usize * image.height as usize * 4
        );
    }
}
