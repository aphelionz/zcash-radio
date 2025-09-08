use anyhow::Result;

use zcash_radio_scan::run;

const TOPIC_URL: &str = "https://forum.zcashcommunity.com/t/what-are-you-listening-to/20456";
const OUT_PATH: &str = "./public/videos.json";

#[tokio::main]
async fn main() -> Result<()> {
    run(TOPIC_URL, OUT_PATH).await?;
    Ok(())
}
