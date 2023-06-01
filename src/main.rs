use twitch_data::prelude::info;
use std::error::Error;
use std::path::Path;

use tokio;

use twitch_data::get_client;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    simplelog::TermLogger::init(
        simplelog::LevelFilter::Info,
        simplelog::Config::default(),
        simplelog::TerminalMode::Mixed,
        simplelog::ColorChoice::Auto,
    )
    .expect("Failed to initialize logger");
    // async fn main() -> Result<(), Box<dyn Error>> {
    println!("Starting!");
    sample().await?;
    println!("Done! 1");
    // get_channel_title_from_login("bananabrea").await?;
    println!("Done! 2");
    // get_video_ids_from_channel("bananabrea").await?;
    println!("Done! 3");
    // get_video_info_from_id("1674543458").await?;
    // get_video_info_from_id("1677206253").await?;
    println!("Done! 4");
    // get_video_playlist("1677206253").await?;
    println!("Done! 5");
    download_video("1768835851").await?;
    println!("Done! 6");
    println!("Done! 7");
    println!("\n\nDone!");
    Ok(())
}

async fn download_video(video_id: &str) -> Result<(), Box<dyn Error>> {
    let client = get_client().await?;
    let path = Path::new("C:\\tmp\\videos\\");
    client.download_video(video_id, "720p60", path).await?;

    Ok(())
}

async fn sample() -> Result<(), Box<dyn Error>> {
    let client = get_client().await?;
    let title = client.get_channel_title_from_login("bananabrea").await?;
    info!("Title: {}", title);
    Ok(())
}
