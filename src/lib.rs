use anyhow::{Context, Result};
use futures::stream::{self, StreamExt};
use regex::Regex;
use reqwest::{Client, StatusCode};
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::fs as tokio_fs;
use tokio::time::sleep;
use url::Url;
use zcash_address::unified::{self, Container, Encoding};
use zcash_protocol::consensus::NetworkType;

static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent("zcash-radio-aphelionz/0.1 (+https://github.com/aphelionz)")
        .build()
        .expect("Failed to build HTTP client")
});

static A_SELECTOR: LazyLock<Selector> = LazyLock::new(|| Selector::parse("a").unwrap());

pub static CURATION_DENYLIST: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("../curation.txt")
        .lines()
        .filter_map(|l| {
            l.split('#')
                .next()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .and_then(|l| l.split('|').next())
                .map(str::trim)
                .filter(|id| !id.is_empty() && is_valid_youtube_id(id))
        })
        .collect()
});

static UA_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)u1[0-9a-z]{10,}").expect("invalid UA regex"));

const CACHE_DIR: &str = "./target/profile_cache";
const CACHE_TTL_SECS: u64 = 24 * 60 * 60;
const PROFILE_CONCURRENCY: usize = 3;
const RETRY_ATTEMPTS: usize = 3;
const RETRY_BASE_DELAY_MS: u64 = 500;

#[derive(Debug, Deserialize)]
pub struct Topic {
    pub post_stream: PostStream,
}

#[derive(Debug, Deserialize)]
pub struct PostStream {
    pub posts: Vec<Post>,
}

#[derive(Debug, Deserialize)]
pub struct Post {
    pub post_number: i64,
    pub cooked: String,
    #[serde(default)]
    pub username: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct VideoEntry {
    pub video_id: String,
    pub source_post_url: String,
    #[serde(default)]
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_unified_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip_has_transparent: Option<bool>,
}

#[derive(Debug, Clone)]
struct TipInfo {
    address: String,
    has_transparent: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedTipEntry {
    cached_at: u64,
    #[serde(default)]
    tip_unified_address: Option<String>,
    #[serde(default)]
    tip_has_transparent: bool,
}

fn cache_path(username: &str) -> PathBuf {
    let mut sanitized = String::with_capacity(username.len());
    for ch in username.chars() {
        if ch.is_ascii_alphanumeric() {
            sanitized.push(ch);
        } else {
            sanitized.push('_');
        }
    }
    Path::new(CACHE_DIR).join(format!("{}.json", sanitized))
}

fn now_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn cache_entry_fresh(entry: &CachedTipEntry) -> bool {
    now_timestamp().saturating_sub(entry.cached_at) <= CACHE_TTL_SECS
}

async fn load_cached_tip(username: &str) -> Option<CachedTipEntry> {
    let path = cache_path(username);
    let data = tokio_fs::read(path).await.ok()?;
    let entry: CachedTipEntry = serde_json::from_slice(&data).ok()?;
    if cache_entry_fresh(&entry) {
        Some(entry)
    } else {
        None
    }
}

async fn store_cached_tip(username: &str, entry: &CachedTipEntry) {
    let path = cache_path(username);
    if let Some(parent) = path.parent() {
        if let Err(err) = tokio_fs::create_dir_all(parent).await {
            eprintln!("cache: failed to create directory: {}", err);
            return;
        }
    }
    match serde_json::to_vec(entry) {
        Ok(buf) => {
            if let Err(err) = tokio_fs::write(&path, buf).await {
                eprintln!("cache: failed to write entry: {}", err);
            }
        }
        Err(err) => {
            eprintln!("cache: failed to serialize entry: {}", err);
        }
    }
}

fn extract_unified_address(text: &str) -> Option<String> {
    UA_REGEX.find(text).map(|m| m.as_str().to_lowercase())
}

fn validate_unified_address(candidate: &str) -> Option<TipInfo> {
    match unified::Address::decode(candidate) {
        Ok((network, address)) => {
            if network != NetworkType::Main {
                eprintln!(
                    "tip: rejected UA on non-mainnet network ({:?}): {}",
                    network, candidate
                );
                return None;
            }
            let has_transparent = address.items().iter().any(|receiver| {
                matches!(
                    receiver,
                    unified::Receiver::P2pkh(_) | unified::Receiver::P2sh(_)
                )
            });
            if has_transparent {
                eprintln!("tip: rejected UA with transparent receiver: {}", candidate);
                None
            } else {
                Some(TipInfo {
                    address: candidate.to_string(),
                    has_transparent,
                })
            }
        }
        Err(err) => {
            eprintln!("tip: invalid UA {}: {}", candidate, err);
            None
        }
    }
}

fn find_address_in_json(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => extract_unified_address(s),
        serde_json::Value::Array(values) => values.iter().find_map(find_address_in_json),
        serde_json::Value::Object(map) => map.values().find_map(find_address_in_json),
        _ => None,
    }
}

async fn fetch_tip_info_with_cache(base_url: &Url, username: &str) -> Option<TipInfo> {
    if username.is_empty() {
        return None;
    }
    if let Some(entry) = load_cached_tip(username).await {
        return entry.tip_unified_address.map(|addr| TipInfo {
            address: addr,
            has_transparent: entry.tip_has_transparent,
        });
    }

    match fetch_tip_info_remote(base_url, username).await {
        Ok(Some(tip)) => {
            let entry = CachedTipEntry {
                cached_at: now_timestamp(),
                tip_unified_address: Some(tip.address.clone()),
                tip_has_transparent: tip.has_transparent,
            };
            store_cached_tip(username, &entry).await;
            Some(tip)
        }
        Ok(None) => {
            let entry = CachedTipEntry {
                cached_at: now_timestamp(),
                tip_unified_address: None,
                tip_has_transparent: false,
            };
            store_cached_tip(username, &entry).await;
            None
        }
        Err(err) => {
            eprintln!("tip: failed to fetch profile: {}", err);
            None
        }
    }
}

async fn fetch_tip_info_remote(base_url: &Url, username: &str) -> Result<Option<TipInfo>> {
    if let Some(info) = fetch_tip_from_json(base_url, username).await? {
        return Ok(Some(info));
    }
    fetch_tip_from_html(base_url, username).await
}

async fn fetch_tip_from_json(base_url: &Url, username: &str) -> Result<Option<TipInfo>> {
    let url = base_url
        .join(&format!("/u/{}.json", username))
        .context("building profile JSON url")?;
    let resp = get_with_retries(&url).await?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        anyhow::bail!("profile json request returned status {}", resp.status());
    }
    let value: serde_json::Value = resp.json().await?;
    if let Some(candidate) = find_address_in_json(&value) {
        if let Some(tip) = validate_unified_address(&candidate) {
            return Ok(Some(tip));
        }
    }
    Ok(None)
}

async fn fetch_tip_from_html(base_url: &Url, username: &str) -> Result<Option<TipInfo>> {
    let url = base_url
        .join(&format!("/u/{}", username))
        .context("building profile HTML url")?;
    let resp = get_with_retries(&url).await?;
    if resp.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !resp.status().is_success() {
        anyhow::bail!("profile html request returned status {}", resp.status());
    }
    let body = resp.text().await?;
    if let Some(candidate) = extract_unified_address(&body) {
        if let Some(tip) = validate_unified_address(&candidate) {
            return Ok(Some(tip));
        }
    }
    Ok(None)
}

async fn get_with_retries(url: &Url) -> Result<reqwest::Response> {
    let mut attempt = 0usize;
    loop {
        match CLIENT.get(url.clone()).send().await {
            Ok(resp) => {
                if should_retry_status(resp.status()) && attempt + 1 < RETRY_ATTEMPTS {
                    let delay = retry_delay(attempt);
                    sleep(delay).await;
                    attempt += 1;
                    continue;
                }
                return Ok(resp);
            }
            Err(err) => {
                if attempt + 1 >= RETRY_ATTEMPTS {
                    return Err(err.into());
                }
                let delay = retry_delay(attempt);
                sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

fn should_retry_status(status: StatusCode) -> bool {
    status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS
}

fn retry_delay(attempt: usize) -> Duration {
    let base = Duration::from_millis(RETRY_BASE_DELAY_MS);
    base * (1u32 << attempt.min(5))
}

pub fn is_valid_youtube_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn extract_video_id(href: &str) -> Option<String> {
    let u = Url::parse(href).ok()?;
    let host = u.host_str()?.to_lowercase();
    let is_youtube = host == "youtu.be" || host.ends_with("youtube.com");
    if !is_youtube {
        return None;
    }

    if host == "youtu.be" {
        if let Some(seg) = u.path_segments().and_then(|mut s| s.next()) {
            if is_valid_youtube_id(seg) {
                return Some(seg.to_string());
            }
        }
        return None;
    }

    let path = u.path();
    if path == "/watch" {
        if let Some(v) = u
            .query_pairs()
            .find(|(k, _)| k == "v")
            .map(|(_, v)| v.into_owned())
        {
            if is_valid_youtube_id(&v) {
                return Some(v);
            }
        }
        return None;
    }

    let rest = match path {
        p if p.starts_with("/shorts/") => Some(&p["/shorts/".len()..]),
        p if p.starts_with("/embed/") => Some(&p["/embed/".len()..]),
        p if p.starts_with("/live/") => Some(&p["/live/".len()..]),
        _ => None,
    };
    if let Some(rest) = rest {
        let id = rest.split('/').next().unwrap_or("");
        if is_valid_youtube_id(id) {
            return Some(id.to_string());
        }
    }

    None
}

pub fn process_posts(
    posts: &[Post],
    topic_url: &str,
    denylist: &HashSet<&str>,
) -> HashMap<String, VideoEntry> {
    let mut map: HashMap<String, VideoEntry> = HashMap::with_capacity(posts.len());
    for p in posts {
        if !p.cooked.contains("youtu") {
            continue;
        }
        let doc = Html::parse_fragment(&p.cooked);
        for a in doc.select(&*A_SELECTOR) {
            if let Some(href) = a.value().attr("href") {
                if !(href.contains("youtu.be") || href.contains("youtube.com")) {
                    continue;
                }
                if let Some(video_id) = extract_video_id(href) {
                    if denylist.contains(video_id.as_str()) {
                        eprintln!("curation: skipped {}", video_id);
                        continue;
                    }
                    let video_id_clone = video_id.clone();
                    match map.entry(video_id) {
                        Entry::Vacant(v) => {
                            v.insert(VideoEntry {
                                video_id: video_id_clone,
                                source_post_url: format!("{}/{}", topic_url, p.post_number),
                                username: p.username.clone(),
                                tip_unified_address: None,
                                tip_has_transparent: None,
                            });
                        }
                        Entry::Occupied(_) => {}
                    }
                }
            }
        }
    }
    map
}

pub async fn run(topic_url: &str, out_path: &str) -> Result<usize> {
    let topic_url = topic_url.trim_end_matches('/');
    let thread_url = Url::parse(topic_url).context("invalid topic url")?;
    let url = format!("{}.json?print=true", topic_url);
    let resp = CLIENT.get(&url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("DISCOURSE ERROR {} -> {}\n{}", url, status, body);
        anyhow::bail!("GET {}", url);
    }
    let topic: Topic = resp.json().await?;
    let posts = topic.post_stream.posts;
    let mut map = process_posts(&posts, topic_url, &CURATION_DENYLIST);

    let usernames: HashSet<String> = map
        .values()
        .filter_map(|entry| {
            let username = entry.username.trim();
            if username.is_empty() {
                None
            } else {
                Some(username.to_string())
            }
        })
        .collect();

    if !usernames.is_empty() {
        let profiles = stream::iter(usernames.into_iter().map(|username| {
            let base = thread_url.clone();
            async move {
                let info = fetch_tip_info_with_cache(&base, &username).await;
                (username, info)
            }
        }))
        .buffer_unordered(PROFILE_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;

        let tip_map: HashMap<String, TipInfo> = profiles
            .into_iter()
            .filter_map(|(username, info)| info.map(|tip| (username, tip)))
            .collect();

        for entry in map.values_mut() {
            let username_key = entry.username.trim();
            if let Some(tip) = tip_map.get(username_key) {
                entry.tip_unified_address = Some(tip.address.clone());
                entry.tip_has_transparent = Some(tip.has_transparent);
            }
        }
    }

    let len = map.len();
    let json = serde_json::to_string_pretty(&map)?;
    fs::write(out_path, json)?;
    eprintln!("Wrote {} unique videos to {}", len, out_path);
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    #[test]
    fn test_is_valid_youtube_id() {
        assert!(is_valid_youtube_id("aaaaaaaaaaa"));
        assert!(!is_valid_youtube_id("short"));
        assert!(!is_valid_youtube_id("invalid$chars"));
    }

    #[test]
    fn test_extract_video_id_various() {
        assert_eq!(
            extract_video_id("https://youtu.be/AAAAAAAAAAA"),
            Some("AAAAAAAAAAA".into())
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/watch?v=BBBBBBBBBBB"),
            Some("BBBBBBBBBBB".into())
        );
        assert_eq!(
            extract_video_id("https://youtube.com/shorts/CCCCCCCCCCC"),
            Some("CCCCCCCCCCC".into())
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/embed/DDDDDDDDDDD/extra"),
            Some("DDDDDDDDDDD".into())
        );
        assert_eq!(
            extract_video_id("https://www.youtube.com/live/EEEEEEEEEEE?feature=share"),
            Some("EEEEEEEEEEE".into())
        );
        assert_eq!(
            extract_video_id("https://example.com/watch?v=AAAAAAAAAAA"),
            None
        );
        assert_eq!(extract_video_id("https://youtu.be/SHORT"), None);
        assert_eq!(
            extract_video_id("https://www.youtube.com/watch?v=invalidid"),
            None
        );
        assert_eq!(extract_video_id("https://www.youtube.com/user/some"), None);
    }

    #[test]
    fn test_process_posts_dedup_and_denylist() {
        let posts = vec![
            Post {
                post_number: 1,
                cooked: "<a href=\"https://youtu.be/AAAAAAAAAAA\">one</a>".into(),
                username: "alice".into(),
            },
            Post {
                post_number: 2,
                cooked: "<a href=\"https://www.youtube.com/watch?v=BBBBBBBBBBB\">two</a>".into(),
                username: "bob".into(),
            },
            Post {
                post_number: 3,
                cooked: "<a href=\"https://youtu.be/BBBBBBBBBBB\">dup</a>".into(),
                username: "carol".into(),
            },
            Post {
                post_number: 4,
                cooked: "<a href=\"https://example.com/video\">nope</a>".into(),
                username: "dave".into(),
            },
        ];
        let denylist: HashSet<&str> = HashSet::from(["AAAAAAAAAAA"]);
        let map = process_posts(&posts, "https://forum", &denylist);
        assert_eq!(map.len(), 1);
        let entry = map.get("BBBBBBBBBBB").unwrap();
        assert_eq!(entry.source_post_url, "https://forum/2");
        assert_eq!(entry.username, "bob");
    }

    #[test]
    fn test_default_denylist_parses() {
        // Ensure the denylist from curation.txt is loaded
        assert!(CURATION_DENYLIST.contains("G7g44Bca1UQ"));
    }

    #[tokio::test]
    async fn test_run_integration() {
        let server = httpmock::MockServer::start();
        let topic_json = serde_json::json!({
            "post_stream": {
                "posts": [{
                    "post_number": 1,
                    "cooked": "<a href=\"https://youtu.be/BBBBBBBBBBB\">v</a>",
                    "username": "alice"
                }]
            }
        });
        server.mock(|when, then| {
            when.method(httpmock::Method::GET)
                .path("/topic.json")
                .query_param("print", "true");
            then.status(200)
                .header("content-type", "application/json")
                .json_body_obj(&topic_json);
        });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("{}/topic", server.base_url());
        let count = run(&url, tmp.path().to_str().unwrap()).await.unwrap();
        assert_eq!(count, 1);
        let data = std::fs::read_to_string(tmp.path()).unwrap();
        let map: HashMap<String, VideoEntry> = serde_json::from_str(&data).unwrap();
        assert!(map.contains_key("BBBBBBBBBBB"));
    }
}
