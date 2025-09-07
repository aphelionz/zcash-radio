use anyhow::Result;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    fs,
};
use url::Url;

const TOPIC_URL: &str = "https://forum.zcashcommunity.com/t/what-are-you-listening-to/20456";
const OUT_PATH: &str = "./public/videos.json";

#[derive(Debug, Deserialize)]
struct Topic {
    post_stream: PostStream,
}

#[derive(Debug, Deserialize)]
struct PostStream {
    posts: Vec<Post>,
}

#[derive(Debug, Deserialize)]
struct Post {
    post_number: i64,
    cooked: String,
    #[serde(default)]
    username: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct VideoEntry {
    video_id: String,
    canonical_url: String,
    source_post_url: String,
    #[serde(default)]
    username: String,
}

fn is_valid_youtube_id(id: &str) -> bool {
    id.len() == 11
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[tokio::main]
async fn main() -> Result<()> {
    let topic_url = TOPIC_URL.trim_end_matches('/');
    let client = reqwest::Client::builder()
        .user_agent("zcash-radio-aphelionz/0.1 (+https://github.com/aphelionz)")
        .build()?;

    let deny = load_curation("curation.txt"); // or from --curation

    // Extract and canonicalize YouTube IDs
    let a_sel = Selector::parse("a").unwrap();

    let url = format!("{}.json?print=true", topic_url);
    let resp = client.get(&url).send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("DISCOURSE ERROR {} -> {}\n{}", url, status, body);
        anyhow::bail!("GET {}", url);
    }
    let topic: Topic = resp.json().await?;
    let posts = topic.post_stream.posts;

    let mut map: HashMap<String, VideoEntry> = HashMap::with_capacity(posts.len());

    let mut process = |p: Post| {
        let doc = Html::parse_fragment(&p.cooked);
        for a in doc.select(&a_sel) {
            if let Some(href) = a.value().attr("href") {
                // Fast path: only consider youtube domains
                if !(href.contains("youtu.be") || href.contains("youtube.com")) {
                    continue;
                }

                // Parse URL and extract a video ID in a robust way
                let video_id_opt = Url::parse(href).ok().and_then(|u| {
                    let host = u.host_str()?.to_lowercase();
                    let is_youtube = host == "youtu.be" || host.ends_with("youtube.com");
                    if !is_youtube {
                        return None;
                    }

                    // youtu.be/<ID>
                    if host == "youtu.be" {
                        if let Some(seg) = u.path_segments().and_then(|mut s| s.next()) {
                            if is_valid_youtube_id(seg) {
                                return Some(seg.to_string());
                            }
                        }
                        return None;
                    }

                    // youtube.com paths
                    let path = u.path();

                    // /watch?v=<ID>
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

                    // /shorts/<ID>, /embed/<ID>, /live/<ID>
                    if let Some(rest) = path
                        .strip_prefix("/shorts/")
                        .or_else(|| path.strip_prefix("/embed/"))
                        .or_else(|| path.strip_prefix("/live/"))
                    {
                        let id = rest.split('/').next().unwrap_or("");
                        if is_valid_youtube_id(id) {
                            return Some(id.to_string());
                        }
                    }

                    None
                });

                if let Some(video_id) = video_id_opt {
                    let canonical = format!("https://www.youtube.com/watch?v={}", video_id);

                    if deny.contains(&video_id) {
                        eprintln!("curation: skipped {}", video_id);
                        continue;
                    }

                    map.entry(video_id.clone()).or_insert_with(|| VideoEntry {
                        video_id: video_id.clone(),
                        canonical_url: canonical.clone(),
                        source_post_url: format!("{}/{}", topic_url, p.post_number),
                        username: p.username.clone(),
                    });
                }
            }
        }
    };

    for post in posts {
        process(post);
    }

    let len = map.len();
    let json = serde_json::to_string_pretty(&map)?;
    fs::write(OUT_PATH, json)?;

    eprintln!("Wrote {} unique videos to {}", len, OUT_PATH);
    Ok(())
}

fn load_curation(path: &str) -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(text) = fs::read_to_string(path) else {
        return set;
    };

    for raw in text.lines() {
        let mut line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // strip inline comment
        if let Some(i) = line.find('#') {
            line = line[..i].trim();
            if line.is_empty() {
                continue;
            }
        }
        // fields: id | reason | source (reason and source ignored)
        if let Some(id) = line.split('|').map(|p| p.trim()).next() {
            if is_valid_youtube_id(id) {
                set.insert(id.to_string());
            }
        }
    }
    set
}
