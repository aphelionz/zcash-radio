use std::collections::HashSet;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use httpmock::MockServer;
use tempfile::tempdir;
use tokio::runtime::Runtime;
use zcash_radio_scan::{process_posts, run, Post};
use std::sync::Arc;

fn bench_process_posts(c: &mut Criterion) {
    // Generate a list of posts with unique YouTube links
    let posts: Vec<Post> = (0..1000)
        .map(|i| {
            let id = format!("ID{:09}", i);
            Post {
                post_number: i as i64,
                cooked: format!("<a href=\"https://youtu.be/{id}\">v</a>"),
                username: format!("user{i}"),
            }
        })
        .collect();
    let denylist: HashSet<&str> = HashSet::new();

    c.bench_function("process_posts", |b| {
        b.iter(|| {
            let map = process_posts(black_box(&posts), black_box("https://forum"), black_box(&denylist));
            black_box(map);
        })
    });
}

fn bench_run_with_mock(c: &mut Criterion) {
    let server = MockServer::start();

    // Sample topic JSON served by the mock server
    let topic_json = serde_json::json!({
        "post_stream": {"posts": [{
            "post_number": 1,
            "cooked": "<a href=\"https://youtu.be/BBBBBBBBBBB\">v</a>",
            "username": "alice"
        }]}
    });

    server.mock(|when, then| {
        when.method(httpmock::Method::GET)
            .path("/topic.json")
            .query_param("print", "true");
        then.status(200)
            .header("content-type", "application/json")
            .json_body_obj(&topic_json);
    });

    let url = Arc::new(format!("{}/topic", server.base_url()));
    let tmp_dir = tempdir().unwrap();
    let out_path = tmp_dir.path().join("videos.json");
    let out_path = Arc::new(out_path.to_str().unwrap().to_string());
    let rt = Runtime::new().unwrap();

    c.bench_function("run_with_mock", |b| {
        let url = Arc::clone(&url);
        let out_path = Arc::clone(&out_path);
        b.to_async(&rt).iter(move || {
            let url = Arc::clone(&url);
            let out_path = Arc::clone(&out_path);
            async move {
                run(black_box(&url), black_box(&out_path))
                    .await
                    .unwrap();
            }
        });
    });
}

criterion_group!(benches, bench_process_posts, bench_run_with_mock);
criterion_main!(benches);
