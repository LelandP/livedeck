//! Chat badge images.
//!
//! The chat server names badges (`moderator/1`, `subscriber/6`) but not their images. Those come
//! from Twitch's web GraphQL API, which needs no login (the same unofficial endpoint the browse
//! screens use before sign-in): one list for all of Twitch, and one per channel
//! (subscriber months, bits tiers). Lines are mapped with whatever is loaded; a badge without an
//! image is left out.
//!
//! The older `badges.twitch.tv` listing no longer exists (its name does not resolve), and the
//! official Helix endpoints need a login, which chat does not have.

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use htpc_twitch_api::{GQL_URL, WEB_CLIENT_ID};
use serde::Deserialize;
use serde_json::json;

/// `(set, version)` to image URL.
pub type BadgeSet = HashMap<(String, String), String>;

/// The badges known for the channel on screen: global ones, overridden by the channel's own.
#[derive(Debug, Default)]
pub struct BadgeBook {
    global: RwLock<BadgeSet>,
    channel: RwLock<BadgeSet>,
}

impl BadgeBook {
    pub fn set_global(&self, set: BadgeSet) {
        *self.global.write().unwrap_or_else(|e| e.into_inner()) = set;
    }

    pub fn set_channel(&self, set: BadgeSet) {
        *self.channel.write().unwrap_or_else(|e| e.into_inner()) = set;
    }

    /// The image of a badge, the channel's own version first.
    pub fn url(&self, set: &str, version: &str) -> Option<String> {
        let key = (set.to_owned(), version.to_owned());
        let channel = self.channel.read().unwrap_or_else(|e| e.into_inner());
        if let Some(url) = channel.get(&key) {
            return Some(url.clone());
        }
        drop(channel);
        self.global
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&key)
            .cloned()
    }
}

#[derive(Deserialize)]
struct Reply {
    data: Option<Data>,
}

#[derive(Deserialize)]
struct Data {
    badges: Option<Vec<Badge>>,
    user: Option<User>,
}

#[derive(Deserialize)]
struct User {
    #[serde(rename = "broadcastBadges")]
    broadcast_badges: Option<Vec<Badge>>,
}

#[derive(Deserialize)]
struct Badge {
    #[serde(rename = "setID")]
    set_id: String,
    version: String,
    #[serde(rename = "imageURL")]
    image_url: Option<String>,
}

fn into_set(badges: Vec<Badge>) -> BadgeSet {
    badges
        .into_iter()
        .filter_map(|b| {
            // Only https images: the URL ends up in a download.
            let url = b.image_url.filter(|u| u.starts_with("https://"))?;
            Some(((b.set_id, b.version), url))
        })
        .collect()
}

/// Reads the global badge answer.
pub fn parse_global(json: &str) -> Result<BadgeSet, String> {
    let reply: Reply = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let badges = reply
        .data
        .and_then(|d| d.badges)
        .ok_or("the answer has no badges")?;
    Ok(into_set(badges))
}

/// Reads a channel's badge answer. A channel without custom badges is an empty set. An unknown
/// channel (`user` is null) is an error.
pub fn parse_channel(json: &str) -> Result<BadgeSet, String> {
    let reply: Reply = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let user = reply.data.and_then(|d| d.user).ok_or("no such channel")?;
    Ok(into_set(user.broadcast_badges.unwrap_or_default()))
}

/// 2x is sharp at the 20 px a badge is drawn at, also on a 2x display.
const GLOBAL_QUERY: &str = "query { badges { setID version imageURL(size: DOUBLE) } }";
const CHANNEL_QUERY: &str = "query($login: String!) { user(login: $login) { broadcastBadges { setID version imageURL(size: DOUBLE) } } }";
const TIMEOUT: Duration = Duration::from_secs(4);

/// Downloads the badge sets from the web GraphQL API.
#[derive(Debug, Clone)]
pub struct HttpBadges {
    client: reqwest::Client,
    endpoint: String,
    client_id: String,
}

impl HttpBadges {
    pub fn new() -> Result<Self, reqwest::Error> {
        Self::with_endpoint(GQL_URL, WEB_CLIENT_ID)
    }

    /// Uses another GraphQL endpoint (tests point it at a mock server).
    pub fn with_endpoint(
        endpoint: impl Into<String>,
        client_id: impl Into<String>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder().timeout(TIMEOUT).build()?;
        Ok(Self {
            client,
            endpoint: endpoint.into(),
            client_id: client_id.into(),
        })
    }

    /// The badges every channel has (moderator, subscriber tiers of Twitch itself, ...).
    pub async fn global(&self) -> Result<BadgeSet, String> {
        let body = json!({ "query": GLOBAL_QUERY });
        parse_global(&self.post(body).await?)
    }

    /// The badges of one channel, by login.
    pub async fn channel(&self, login: &str) -> Result<BadgeSet, String> {
        if login.is_empty() {
            return Err("no channel".into());
        }
        let body = json!({ "query": CHANNEL_QUERY, "variables": { "login": login } });
        parse_channel(&self.post(body).await?)
    }

    async fn post(&self, body: serde_json::Value) -> Result<String, String> {
        self.client
            .post(&self.endpoint)
            .header("Client-Id", &self.client_id)
            .json(&body)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?
            .text()
            .await
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GLOBAL: &str = r#"{"data":{"badges":[
        {"setID":"moderator","version":"1","imageURL":"https://cdn/mod2.png"},
        {"setID":"subscriber","version":"0","imageURL":"https://cdn/sub0.png"},
        {"setID":"insecure","version":"1","imageURL":"http://cdn/x.png"},
        {"setID":"imageless","version":"1","imageURL":null}]}}"#;

    const CHANNEL: &str = r#"{"data":{"user":{"broadcastBadges":[
        {"setID":"subscriber","version":"6","imageURL":"https://cdn/sub6.png"}]}}}"#;

    fn get<'a>(set: &'a BadgeSet, s: &str, v: &str) -> Option<&'a str> {
        set.get(&(s.to_owned(), v.to_owned())).map(String::as_str)
    }

    #[test]
    fn the_global_answer_maps_set_and_version_to_an_https_image() {
        let set = parse_global(GLOBAL).unwrap();
        assert_eq!(get(&set, "moderator", "1"), Some("https://cdn/mod2.png"));
        assert_eq!(get(&set, "subscriber", "0"), Some("https://cdn/sub0.png"));
        // No image, or not https: left out.
        assert_eq!(get(&set, "insecure", "1"), None);
        assert_eq!(get(&set, "imageless", "1"), None);
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn a_channel_answer_can_be_empty_but_an_unknown_channel_is_an_error() {
        let set = parse_channel(CHANNEL).unwrap();
        assert_eq!(get(&set, "subscriber", "6"), Some("https://cdn/sub6.png"));
        let none = parse_channel(r#"{"data":{"user":{"broadcastBadges":null}}}"#).unwrap();
        assert!(none.is_empty());
        assert!(parse_channel(r#"{"data":{"user":null}}"#).is_err());
    }

    #[test]
    fn broken_answers_are_errors_not_panics() {
        for bad in [
            "",
            "{",
            "[]",
            r#"{"data":null}"#,
            r#"{"errors":[{"message":"x"}]}"#,
        ] {
            assert!(parse_global(bad).is_err(), "{bad:?}");
            assert!(parse_channel(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn channel_badges_win_over_global_ones() {
        let book = BadgeBook::default();
        let one = |url: &str| BadgeSet::from([(("subscriber".into(), "0".into()), url.into())]);
        book.set_global(one("https://cdn/global.png"));
        assert_eq!(
            book.url("subscriber", "0").as_deref(),
            Some("https://cdn/global.png")
        );
        book.set_channel(one("https://cdn/channel.png"));
        assert_eq!(
            book.url("subscriber", "0").as_deref(),
            Some("https://cdn/channel.png")
        );
        assert_eq!(book.url("subscriber", "9"), None);
    }

    #[tokio::test]
    async fn both_sets_are_asked_for_with_the_web_client_id() {
        use wiremock::matchers::{body_string_contains, header, method};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(header("Client-Id", "web-id"))
            .and(body_string_contains("broadcastBadges"))
            .and(body_string_contains("\"login\":\"chan\""))
            .respond_with(ResponseTemplate::new(200).set_body_string(CHANNEL))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(header("Client-Id", "web-id"))
            .and(body_string_contains("query { badges"))
            .respond_with(ResponseTemplate::new(200).set_body_string(GLOBAL))
            .mount(&server)
            .await;

        let badges = HttpBadges::with_endpoint(server.uri(), "web-id").unwrap();
        assert_eq!(badges.global().await.unwrap().len(), 2);
        let channel = badges.channel("chan").await.unwrap();
        assert_eq!(
            get(&channel, "subscriber", "6"),
            Some("https://cdn/sub6.png")
        );
    }

    #[tokio::test]
    async fn a_server_error_is_reported() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let badges = HttpBadges::with_endpoint(server.uri(), "id").unwrap();
        assert!(badges.global().await.is_err());
        assert!(badges.channel("chan").await.is_err());
        assert!(badges.channel("").await.is_err());
    }
}
