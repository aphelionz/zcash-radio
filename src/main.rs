use anyhow::{Context, Result};
use clap::Parser;
use regex::Regex;
use scraper::{Html, Selector};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::PathBuf};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};
use url::Url;

#[derive(Parser, Debug)]
#[command(name = "zcash-radio-scan", version)]
struct Args {
    /// Discourse topic URL (no .json)
    #[arg(
        long,
        default_value = "https://forum.zcashcommunity.com/t/what-are-you-listening-to/20456"
    )]
    topic_url: String,

    /// Output JSON file (will be created/updated)
    #[arg(long, default_value = "./public/videos.json")]
    out: PathBuf,

    /// Use chunked fetching via post_ids (safety valve if print=true ever fails)
    #[arg(long, default_value_t = false)]
    chunked: bool,
}

#[derive(Debug, Deserialize)]
struct Topic {
    post_stream: PostStream,
}

#[derive(Debug, Deserialize)]
struct PostStream {
    posts: Vec<Post>,
    #[serde(default)]
    stream: Vec<i64>,
}

#[derive(Debug, Deserialize)]
struct Post {
    id: i64,
    post_number: i64,
    cooked: String,
    #[serde(default)]
    created_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct VideoEntry {
    video_id: String,
    canonical_url: String,
    first_seen_post: i64,
    first_seen_post_number: i64,
    source_post_url: String,
    last_seen_at: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let client = reqwest::Client::builder()
        .user_agent("zcash-radio/0.1 (+https://github.com/you)")
        .build()?;

    let posts = if !args.chunked {
        fetch_topic_print(&client, &args.topic_url).await?
    } else {
        fetch_topic_chunked(&client, &args.topic_url).await?
    };

    // Extract and canonicalize YouTube IDs
    let a_sel = Selector::parse("a").unwrap();
    let id_pat = Regex::new(r"^[A-Za-z0-9_-]{11}$").unwrap();

    let mut map: BTreeMap<String, VideoEntry> = if args.out.exists() {
        let data = fs::read_to_string(&args.out)?;
        serde_json::from_str(&data).unwrap_or_default()
    } else {
        BTreeMap::new()
    };

    for p in posts {
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
                            if id_pat.is_match(seg) {
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
                            if id_pat.is_match(&v) {
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
                        if id_pat.is_match(id) {
                            return Some(id.to_string());
                        }
                    }

                    None
                });

                if let Some(video_id) = video_id_opt {
                    let canonical = format!("https://www.youtube.com/watch?v={}", video_id);
                    let now = OffsetDateTime::now_utc().format(&Rfc3339).unwrap();

                    let entry = map.entry(video_id.clone()).or_insert_with(|| VideoEntry {
                        video_id: video_id.clone(),
                        canonical_url: canonical.clone(),
                        first_seen_post: p.id,
                        first_seen_post_number: p.post_number,
                        source_post_url: format!("{}/{}", args.topic_url, p.post_number),
                        last_seen_at: now.clone(),
                    });
                    entry.last_seen_at = now;
                }
            }
        }
    }

    // Persist deterministically (sorted by key)
    let json = serde_json::to_string_pretty(&map)?;
    fs::write(&args.out, json)?;

    eprintln!(
        "Upserted {} unique videos into {}",
        map.len(),
        args.out.display()
    );
    Ok(())
}

async fn fetch_topic_print(client: &reqwest::Client, topic_url: &str) -> Result<Vec<Post>> {
    let url = format!("{}.json?print=true", topic_url.trim_end_matches('/'));
    let topic: Topic = client
        .get(&url)
        .send()
        .await?
        .error_for_status()
        .with_context(|| format!("GET {}", url))?
        .json()
        .await?;
    Ok(topic.post_stream.posts)
}

// Safety valve: fetch first page, then chunk via /t/{id}/posts.json?post_ids[]=...
async fn fetch_topic_chunked(client: &reqwest::Client, topic_url: &str) -> Result<Vec<Post>> {
    let base = format!("{}.json", topic_url.trim_end_matches('/'));
    let t: Topic = client
        .get(&base)
        .send()
        .await?
        .error_for_status()
        .with_context(|| format!("GET {}", base))?
        .json()
        .await?;

    let mut posts = t.post_stream.posts;
    let mut ids = t.post_stream.stream;

    // Drop first 20 (already included)
    if ids.len() >= 20 {
        ids.drain(0..20);
    } else {
        ids.clear();
    }

    for chunk in ids.chunks(20) {
        let mut url = format!("{}/posts.json?", topic_url.trim_end_matches('/'));
        for id in chunk {
            url.push_str(&format!("post_ids[]={}&", id));
        }
        let got: serde_json::Value = client
            .get(&url)
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("GET {}", url))?
            .json()
            .await?;

        if let Some(arr) = got
            .get("post_stream")
            .and_then(|ps| ps.get("posts"))
            .and_then(|p| p.as_array())
        {
            posts.extend(
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok()),
            );
        } else if let Some(arr) = got.get("posts").and_then(|p| p.as_array()) {
            posts.extend(
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok()),
            );
        }

        tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    }
    Ok(posts)
}
