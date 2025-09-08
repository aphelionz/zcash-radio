use anyhow::Result;
use reqwest::Client;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use serde_json;
use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::fs;
use std::sync::LazyLock;
use url::Url;

static CLIENT: LazyLock<Client> = LazyLock::new(|| {
    Client::builder()
        .user_agent("zcash-radio-aphelionz/0.1 (+https://github.com/aphelionz)")
        .build()
        .expect("Failed to build HTTP client")
});

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
    let a_sel = Selector::parse("a").unwrap();
    let mut map: HashMap<String, VideoEntry> = HashMap::with_capacity(posts.len());
    for p in posts {
        let doc = Html::parse_fragment(&p.cooked);
        for a in doc.select(&a_sel) {
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
    let map = process_posts(&posts, topic_url, &CURATION_DENYLIST);
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
