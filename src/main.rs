#[allow(unused, dead_code)]
use std::error::Error;
use std::path::Path;
use std::path::PathBuf;
use tracing::info;
use twba_twitch_data::combine_parts_into_single_ts;
use twba_twitch_data::convert_ts_to_mp4;
use twba_twitch_data::get_client;
use twba_twitch_data::sort_video_part_filenames;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    simplelog::TermLogger::init(
        simplelog::LevelFilter::Info,
        simplelog::Config::default(),
        simplelog::TerminalMode::Mixed,
        simplelog::ColorChoice::Always,
    )
    .expect("Failed to initialize logger");
    // async fn main() -> Result<(), Box<dyn Error>> {
    println!("Starting!");
    info!("Starting!");
    sample().await?;
    info!("Done! 1");
    // get_channel_title_from_login("bananabrea").await?;
    info!("Done! 2");
    // get_video_ids_from_channel("bananabrea").await?;
    info!("Done! 3");
    // get_video_info_from_id("1674543458").await?;
    // get_video_info_from_id("1677206253").await?;
    info!("Done! 4");
    // get_video_playlist("1677206253").await?;
    info!("Done! 5");
    // download_video("1768835851").await?;
    // download_video("1835211564").await?;
    // download_video("1792000647").await?;
    // combine_parts_test("1792000647").await?;
    info!("Done! 6");
    is_channel_live().await?;
    info!("Done! 7");
    info!("\n\nDone!");
    println!("DONE!");
    Ok(())
}

async fn is_channel_live() -> Result<(), Box<dyn Error>> {
    let client = get_client().await?;
    let is_live = client.is_live("spaceboy").await?;
    info!("Is live: {}", is_live);
    Ok(())
}

async fn download_video(video_id: &str) -> Result<(), Box<dyn Error>> {
    let client = get_client().await?;
    // let path = Path::new("C:\\tmp\\videos\\");
    let path = Path::new("/var/tmp/twba/videos");

    // client.download_video(video_id, "160p30", path).await?;
    client
        .download_video(video_id, "max (this does not actually do anything)", path)
        .await?;

    Ok(())
}

async fn sample() -> Result<(), Box<dyn Error>> {
    let client = get_client().await?;
    let title = client.get_channel_title_from_login("bananabrea").await?;
    info!("Title: {}", title);
    Ok(())
}

async fn combine_parts_test(video_id: &str) -> Result<(), Box<dyn Error>> {
    let folder_path = format!("/var/tmp/twba/videos/{}", video_id);
    let target_path = format!("{}/video.ts", folder_path);
    let target_path_mp4 = format!("{}/video.mp4", folder_path);
    let target_path = Path::new(&target_path);
    if target_path.exists() {
        tokio::fs::remove_file(target_path).await?;
    }
    let mut paths = tokio::fs::read_dir(folder_path)
        .await
        .expect("could not read dir");
    let mut next = paths.next_entry().await?;
    let mut tmp = vec![];
    while let Some(path) = next {
        next = paths.next_entry().await?;
        if path.file_name() != "video.ts" {
            println!("path: {:?}", path);
            tmp.push(path.path());
        }
    }
    let mut paths = tmp;

    sort_video_part_filenames(video_id, &mut paths);
    combine_parts_into_single_ts(paths, &target_path.to_path_buf()).await?;
    convert_ts_to_mp4(&PathBuf::from(target_path_mp4), &target_path.to_path_buf()).await?;
    Ok(())
}
