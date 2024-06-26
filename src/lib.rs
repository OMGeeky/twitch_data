use std::{
    collections::HashMap,
    error::Error as StdError,
    fmt::Debug,
    path::{Path, PathBuf},
    result::Result as StdResult,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use exponential_backoff::twitch::{
    check_backoff_twitch_get, check_backoff_twitch_get_with_client,
    check_backoff_twitch_with_client,
};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio::time::Duration;
use tokio::time::Instant;
use twba_backup_config::prelude::Config;
use twba_backup_config::Conf;
use twitch_api::helix::channels::ChannelInformation;
use twitch_api::helix::videos::Video as TwitchVideo;
use twitch_api::types::{Timestamp, VideoPrivacy};
use twitch_oauth2::{ClientId, ClientSecret};
pub use twitch_types::{UserId, VideoId};

#[allow(unused, dead_code)]
//^^ hides some warnings while developing TODO: remove at release
use crate::prelude::*;

pub mod prelude;

//region DownloadError
#[derive(Debug, Error)]
pub enum DownloadError {
    #[error("Error while downloading all parts")]
    DownloadAllParts,
    #[error("Error with the canonicalization of the path")]
    Canonicalization,
    #[error("No parts found")]
    NoParts,
    #[error("No channel info found")]
    NoChannelFound,
    #[error("Error while running ffmpeg command after {0:?}: {1}")]
    Ffmpeg(Duration, anyhow::Error),
    #[error(transparent)]
    Other(#[from] Box<dyn StdError>),
}

//endregion

type Result<T> = std::result::Result<T, Box<dyn StdError>>;

//region Proxies
#[derive(Debug)]
pub struct Video {
    pub id: String,
    pub title: String,
    pub description: String,
    pub user_login: String,
    pub created_at: DateTime<Utc>,
    pub url: String,
    pub viewable: String,
    pub language: String,
    pub view_count: i64,
    pub video_type: String,
    pub duration: i64,
    pub thumbnail_url: String,
}

pub fn convert_twitch_video_to_twitch_data_video(twitch_video: TwitchVideo) -> Video {
    let viewable = match twitch_video.viewable {
        VideoPrivacy::Public => "public",
        VideoPrivacy::Private => "private",
    }
    .to_string();
    Video {
        id: twitch_video.id.into(),
        title: twitch_video.title,
        description: twitch_video.description,
        user_login: twitch_video.user_name.into(),
        created_at: convert_twitch_timestamp(twitch_video.created_at),
        url: twitch_video.url,
        viewable,
        language: twitch_video.language,
        view_count: twitch_video.view_count,
        video_type: "archive".to_string(),
        duration: convert_twitch_duration(&twitch_video.duration).num_seconds() as i64,
        thumbnail_url: twitch_video.thumbnail_url,
    }
}

impl<'a> TwitchClient<'a> {
    pub async fn is_live(&self, login: &str) -> Result<bool> {
        use twitch_api::helix::streams::get_streams;
        let login = login.into();
        let logins = &[login];
        let mut req = get_streams::GetStreamsRequest::user_logins(logins);
        req.first = Some(1);
        let no_stream_found = self
            .client
            .helix
            .req_get(req, &self.token)
            .await?
            .data
            .is_empty();
        Ok(!no_stream_found)
    }

    pub async fn get_videos_from_login(
        &self,
        login: &str,
        limit: Option<usize>,
    ) -> Result<Vec<Video>> {
        let limit = limit.unwrap_or(10000);
        let user_id = self.get_channel_id_from_login(login).await?;
        let v = self.get_videos_from_channel(&user_id, limit).await?;
        let v = v
            .into_iter()
            .map(convert_twitch_video_to_twitch_data_video)
            .collect();
        Ok(v)
    }
}

//endregion Proxies
pub enum VideoQuality {
    Source,
    VeryHigh,
    High,
    Medium,
    Low,
    AudioOnly,
    Other(String),
}

pub struct TwitchClient<'a> {
    reqwest_client: reqwest::Client,
    client: twitch_api::TwitchClient<'a, reqwest::Client>,
    token: twitch_oauth2::AppAccessToken,
    conf: Conf,
}

//region getting general infos
impl<'a> TwitchClient<'a> {
    pub async fn new(
        client_id: ClientId,
        client_secret: ClientSecret,
        conf: Conf,
    ) -> Result<TwitchClient<'a>> {
        trace!("Creating new TwitchClient");
        let reqwest_client = reqwest::Client::new();
        debug!("Created new reqwest client");
        debug!("Creating new twitch_api::TwitchClient");
        let client: twitch_api::TwitchClient<reqwest::Client> = twitch_api::TwitchClient::default();
        let token = twitch_oauth2::AppAccessToken::get_app_access_token(
            &client,
            client_id,
            client_secret,
            twitch_oauth2::Scope::all(),
        )
        .await?;
        debug!("Created new twitch_oauth2::AppAccessToken: {:?}", token);
        let res = Self {
            client,
            token,
            reqwest_client,
            conf,
        };
        trace!("Created new TwitchClient");
        Ok(res)
    }

    //region channels

    pub async fn get_channel_id_from_login(&self, channel_login: &str) -> Result<UserId> {
        let info = self.get_channel_info_from_login(channel_login).await?;

        let info = info.unwrap();

        Ok(info.broadcaster_id)
    }

    pub async fn get_channel_title_from_login(&self, channel_login: &str) -> Result<String> {
        trace!("Getting channel title from login: {}", channel_login);
        let result = self.get_channel_info_from_login(channel_login).await?;
        if let Some(channel_info) = result {
            Ok(channel_info.title.clone())
        } else {
            Err(Box::new(DownloadError::NoChannelFound))
        }
    }

    pub async fn get_channel_info_from_login(
        &self,
        channel_login: &str,
    ) -> Result<Option<ChannelInformation>> {
        trace!("Getting channel info from login: {}", channel_login);
        let res = self
            .client
            .helix
            .get_channel_from_login(channel_login, &self.token)
            // .req_get(GetChannelInformationRequest::log(ids), &self.token)
            .await?;

        Ok(res)
    }

    //endregion

    //region videos

    pub async fn get_video_ids_from_login<S: Into<String>>(
        &self,
        login: S,
        max_results: usize,
    ) -> Result<Vec<VideoId>> {
        let login = login.into();
        trace!("Getting video ids from login: {}", login);
        let id = self.get_channel_id_from_login(&login).await?;
        self.get_video_ids_from_channel(&id, max_results).await
    }

    pub async fn get_video_ids_from_channel(
        &self,
        channel: &UserId,
        max_results: usize,
    ) -> Result<Vec<VideoId>> {
        trace!("Getting video ids from channel");
        let res = self.get_videos_from_channel(channel, max_results).await?;
        Ok(res.into_iter().map(|v| v.id).collect())
    }

    pub async fn get_videos_from_channel(
        &self,
        channel: &UserId,
        max_results: usize,
    ) -> Result<Vec<TwitchVideo>> {
        trace!("Getting videos from channel");
        let mut request = twitch_api::helix::videos::GetVideosRequest::user_id(channel);
        if max_results <= 100 {
            request.first = Some(max_results);
        }
        let res = self
            .client
            .helix
            .req_get(request.clone(), &self.token)
            .await?;
        let mut data: Vec<twitch_api::helix::videos::Video> = res.data.clone();

        let mut count = data.len();
        if count < max_results {
            let mut next = res.get_next(&self.client.helix, &self.token).await?;
            'loop_pages: while count < max_results {
                if let Some(n) = next {
                    for element in &n.data {
                        if count >= max_results {
                            break 'loop_pages;
                        }
                        data.push(element.clone());
                        count += 1;
                    }

                    next = n.get_next(&self.client.helix, &self.token).await?;
                } else {
                    break 'loop_pages;
                }
            }
        }
        // let data = data
        //     .into_iter()
        //     .map(convert_twitch_video_to_twitch_data_video)
        //     .collect();

        Ok(data)
    }

    pub async fn get_video_info(&self, video_id: &VideoId) -> Result<TwitchVideo> {
        trace!("Getting video info");
        let ids = vec![video_id.as_ref()];
        let request = twitch_api::helix::videos::GetVideosRequest::ids(ids);
        let res = self.client.helix.req_get(request, &self.token).await?;
        let video = res.data.into_iter().next().unwrap();
        Ok(video)
    }

    //endregion
}
//endregion

//region downloading videos / getting infos for downloading
impl<'a> TwitchClient<'a> {
    async fn get_video_token_and_signature<S: Into<String>>(
        &self,
        video_id: S,
    ) -> Result<(String, String)> {
        let video_id = video_id.into();
        trace!(
            "Getting video token and signature for video id: {}",
            video_id
        );
        let json = r#"{
  "operationName": "PlaybackAccessToken_Template",
  "query": "query PlaybackAccessToken_Template($login: String!, $isLive: Boolean!, $vodID: ID!, $isVod: Boolean!, $playerType: String!) {  streamPlaybackAccessToken(channelName: $login, params: {platform: \"web\", playerBackend: \"mediaplayer\", playerType: $playerType}) @include(if: $isLive) {    value    signature    __typename  }  videoPlaybackAccessToken(id: $vodID, params: {platform: \"web\", playerBackend: \"mediaplayer\", playerType: $playerType}) @include(if: $isVod) {    value    signature    __typename  }}",
  "variables": {
    "isLive": false,
    "login": "",
    "isVod": true,
    "vodID": ""#.to_string()
            + &video_id
            + r#"",
    "playerType": "embed"
  }
}"#;

        let url = "https://gql.twitch.tv/gql";
        let config = &self.conf;

        trace!("Sending request to: {}", url);
        debug!("Request body: {}", json);
        debug!("Client-ID: {}", &config.twitch.client_id);
        let request = self
            .reqwest_client
            .post(url)
            .header("Client-ID", &config.twitch.client_id)
            .body(json)
            .build()?;
        let res = check_backoff_twitch_with_client(request, &self.reqwest_client).await?;
        // let res = self.reqwest_client.execute(request).await?;
        trace!("get_video_token_and_signature: Got response: {:?}", res);
        let j = res.text().await?;
        debug!("get_video_token_and_signature: Response body: {}", j);
        let json: TwitchVideoAccessTokenResponse = serde_json::from_str(&j)?;
        trace!(
            "get_video_token_and_signature: Got video token and signature: {:?}",
            json
        );
        Ok((
            json.data.video_playback_access_token.value,
            json.data.video_playback_access_token.signature,
        ))
    }
    //noinspection HttpUrlsUsage
    async fn get_video_playlist<S: Into<String>>(
        &self,
        video_id: S,
        token: S,
        signature: S,
    ) -> Result<String> {
        let video_id = video_id.into();
        let token = token.into();
        let signature = signature.into();

        let playlist_url = format!(
            "http://usher.ttvnw.net/vod/{}?nauth={}&nauthsig={}&allow_source=true&player=twitchweb",
            video_id, token, signature
        );
        let playlist = check_backoff_twitch_get(playlist_url).await?.text().await?;
        // let playlist = reqwest::get(playlist_url).await?.text().await?;

        Ok(playlist)
    }
    async fn get_video_playlist_with_quality<S: Into<String>>(
        &self,
        video_id: S,
        quality: S,
    ) -> Result<String> {
        let video_id = video_id.into();
        let quality = quality.into();
        trace!(
            "Getting video playlist with quality for video {} with quality {}",
            video_id,
            quality
        );

        let (token, signature) = self.get_video_token_and_signature(&video_id).await?;
        debug!("Got token and signature: {}, {}", token, signature);
        let playlist = self
            .get_video_playlist(&video_id, &token, &signature)
            .await?;
        debug!("Got playlist: {}", playlist);
        let mut qualties = HashMap::new();

        let mut highest_quality = String::new();
        let test: Vec<&str> = playlist.lines().collect();
        for (i, line) in test.iter().enumerate() {
            if !line.contains("#EXT-X-MEDIA") {
                continue;
            }

            // lastPart = current_row[current_row.index('NAME="') + 6:]
            // stringQuality = lastPart[0:lastPart.index('"')]
            //
            // if videoQualities.get(stringQuality) is not None:
            //     continue
            //
            // videoQualities[stringQuality] = playlist[i + 2]
            let q = line.split("NAME=\"").collect::<Vec<&str>>()[1]
                .split('"')
                .collect::<Vec<&str>>()[0];

            if qualties.get(q).is_some() {
                continue;
            }
            let url = test[i + 2];
            if qualties.len() == 0 {
                highest_quality = q.to_string();
            }

            qualties.insert(q, url);
        }

        if qualties.contains_key(quality.as_str()) {
            Ok(qualties.get(quality.as_str()).unwrap().to_string())
        } else {
            println!(
                "Given quality not found ({}), using highest quality: {}",
                quality, highest_quality
            );
            Ok(qualties.get(highest_quality.as_str()).unwrap().to_string())
        }
        // let url = match qualties.get(quality.as_str()) {
        //     Some(url) => url.to_string(),
        //     None => {
        //         highest_quality
        //     }
        // };
        //
        // Ok(url)
    }

    pub async fn download_video_by_id(
        &self,
        video_id: &VideoId,
        quality: &VideoQuality,
        output_folder_path: &Path,
    ) -> Result<PathBuf> {
        let id: &str = &video_id.as_str();

        let quality: String = match quality {
            VideoQuality::Source => String::from("source"),
            VideoQuality::VeryHigh => String::from("1440p60"),
            VideoQuality::High => String::from("1080p60"),
            VideoQuality::Medium => String::from("720p60"),
            VideoQuality::Low => String::from("480p60"),
            VideoQuality::AudioOnly => todo!("Audio only not supported yet"),
            VideoQuality::Other(v) => v.to_string(),
        };

        return self.download_video(id, quality, output_folder_path).await;
    }
    //TODO: create a function that downloads partial videos (all parts from ... to ... of that video)
    //TODO: also create a function that returns an iterator? that iterates over all partial downloads of the video so they can be uploaded separately
    pub async fn download_video<S1: Into<String>, S2: Into<String>>(
        &self,
        video_id: S1,
        quality: S2,
        output_folder_path: &Path,
    ) -> Result<PathBuf> {
        let video_id = video_id.into();
        trace!("Downloading video: {}", video_id);
        let quality = quality.into();
        let folder_path = output_folder_path.join(&video_id);

        //get parts
        let url = self
            .get_video_playlist_with_quality(&video_id, &quality)
            .await?;

        info!("downloading all parts of video: {}", url);
        let files = self.download_all_parts(&url, &folder_path).await?;
        info!("downloaded all parts of video: {}", files.len());

        //combine parts

        let mut files = files
            .into_iter()
            .enumerate()
            .map(|(i, file)| {
                trace!("file: {} {:?}", i, file.as_ref().map(|x| x.display()));
                if let Some(file) = file {
                    let file = file.canonicalize();
                    if let Ok(file) = file {
                        Ok(file)
                    } else {
                        Err(DownloadError::Canonicalization)
                    }
                } else {
                    Err(DownloadError::DownloadAllParts)
                }
            })
            .collect::<StdResult<Vec<_>, DownloadError>>()?;

        sort_video_part_filenames(&video_id, &mut files);

        let video_ts = output_folder_path.join(&video_id).join("video.ts");
        let video_mp4 = output_folder_path.join(&video_id).join("video.mp4");
        combine_parts_into_single_ts(files, &video_ts).await?;
        convert_ts_to_mp4(&video_mp4, &video_ts).await?;
        info!("done converting to mp4");

        debug!("removing temporary files");
        let final_path = output_folder_path.join(format!("{}.mp4", video_id));
        tokio::fs::rename(&video_mp4, &final_path).await?;
        tokio::fs::remove_dir_all(folder_path).await?;
        debug!("done removing temporary files");
        Ok(final_path)
    }

    async fn download_all_parts(
        &self,
        url: &String,
        folder_path: &PathBuf,
    ) -> Result<Vec<Option<PathBuf>>> {
        trace!("downloading all parts of video: {}", url);
        let config = &self.conf;
        let mut amount_of_threads = config.twitch.downloader_thread_count as usize;
        let base_url = get_base_url(url);
        info!("getting parts");
        let (age, parts) = self.get_parts(url).await?;
        let try_unmute = age < 24;
        info!("getting parts ...Done");

        let amount_of_parts = parts.len();
        info!("part count: {}", amount_of_parts);
        if amount_of_parts < 1 {
            return Err(Box::new(DownloadError::NoParts));
        }

        //download parts
        std::fs::create_dir_all(folder_path)?;

        info!("downloading parts");

        if amount_of_threads < 1 {
            amount_of_threads = 1
        } else if amount_of_threads > amount_of_parts {
            amount_of_threads = amount_of_parts;
        }
        let (completed, progress_done, progress_handle) = Self::create_progress_indicator(
            amount_of_parts,
            Duration::from_secs(5),
            "Downloading Parts",
        );
        let files = futures::stream::iter(parts.into_iter().map(|part| {
            let folder_path = folder_path.clone();
            let url = base_url.clone();
            async {
                let result = download_part(part, url, folder_path, try_unmute).await;
                completed.fetch_add(1, Ordering::Relaxed);
                result
            }
        }))
        .buffer_unordered(amount_of_threads)
        .collect::<Vec<Option<PathBuf>>>();
        let files = files.await;
        info!("downloaded parts");
        //tell the progress indicator to stop and wait for it to exit
        progress_done.fetch_or(true, Ordering::Relaxed);
        let _ = progress_handle.await;

        info!("downloaded all parts of the video");
        Ok(files)
    }

    fn create_progress_indicator(
        amount_of_parts: usize,
        report_frequency: Duration,
        title: impl Into<String>,
    ) -> (Arc<AtomicUsize>, Arc<AtomicBool>, JoinHandle<()>) {
        let completed = Arc::new(AtomicUsize::new(0));
        let canceled = Arc::new(AtomicBool::new(false));
        let title = title.into();
        let progress_handle = {
            let completed = Arc::clone(&completed);
            let canceled = Arc::clone(&canceled);
            tokio::spawn(async move {
                while !canceled.load(Ordering::Relaxed) {
                    let current_progress = completed.load(Ordering::Relaxed);
                    info!(
                        "{}: {:>6.2}% ({}/{}) [{}]",
                        title,
                        (current_progress as f64 / amount_of_parts as f64) * 100.0,
                        current_progress,
                        amount_of_parts,
                        Arc::strong_count(&completed)
                    );
                    tokio::time::sleep(report_frequency).await;
                    // sleep for a while
                }
                let current_progress = completed.load(Ordering::Relaxed);
                info!(
                    "{} Completed! ({}/{})",
                    title, current_progress, amount_of_parts
                );
            })
        };
        (completed, canceled, progress_handle)
    }

    async fn get_parts(&self, url: &String) -> Result<(u64, HashMap<String, f32>)> {
        // let response = self.reqwest_client.get(url).send().await?;
        trace!("getting parts from url: {}", url);
        let response = check_backoff_twitch_get_with_client(url, &self.reqwest_client).await?;
        let video_chunks = response.text().await?;
        trace!("got parts: {}", video_chunks.len());
        trace!("video_chunks: \n\n{}\n", video_chunks);
        let lines = video_chunks.lines().collect::<Vec<&str>>();

        let mut age = 25;
        for line in &lines {
            if !line.starts_with("#ID3-EQUIV-TDTG:") {
                continue;
            }
            let time = line.split("ID3-EQUIV-TDTG:").collect::<Vec<&str>>()[1];
            let time = convert_twitch_time(time);
            let now = Utc::now();
            let diff = now - time;
            age = diff.num_seconds() as u64 / 3600;
            break;
        }

        let mut parts = HashMap::new();

        for i in 0..lines.len() {
            trace!("line: {}", i);
            let l0 = lines[i];
            if !l0.contains("#EXTINF") {
                continue;
            }

            let l1 = lines[i + 1];
            if !l1.contains("#EXT-X-BYTERANGE") {
                let v = l0[8..].strip_suffix(",").unwrap().parse::<f32>().unwrap();
                parts.insert(l1.to_string(), v);
                trace!("no byterange found: {i}");
                continue;
            }

            let l2 = lines[i + 2];
            if parts.contains_key(l2) {
                // # There might be code here but I think its useless
            } else {
                let v = l0[8..].strip_suffix(",").unwrap().parse::<f32>().unwrap();
                parts.insert(l2.to_string(), v);
            }
            info!(
                "i: {}; videoChunks[i + 2]: {}; videoChunks[i]: {}",
                i, l2, l0
            )
        }
        Ok((age, parts))
    }
}

async fn download_part(
    part: (String, f32),
    url: String,
    main_path: PathBuf,
    try_unmute: bool,
) -> Option<PathBuf> {
    trace!("downloading part: {:?}", part);
    let (part, _duration) = part;
    let part_url = format!("{}{}", url, part);
    let part_url_unmuted = format!("{}{}", url, part.replace("-muted", ""));

    if !try_unmute {
        trace!("not to unmute part: {}", part_url);
        return try_download(&main_path, &part, &part_url).await;
    }
    trace!("trying to download unmuted part: {}", part_url_unmuted);
    match try_download(&main_path, &part, &part_url_unmuted).await {
        Some(path) => Some(path),
        None => {
            trace!("failed to download unmuted part. downloading muted part");
            try_download(&main_path, &part, &part_url).await //TODO: check if this is the right error for a failed unmute
        }
    }
}

async fn try_download(main_path: &PathBuf, part: &String, part_url: &String) -> Option<PathBuf> {
    trace!("trying to download part: {}", part_url);
    let path = Path::join(main_path, part);

    let mut res = check_backoff_twitch_get(part_url).await.ok()?;
    // let mut res = reqwest::get(part_url).await?;
    if !res.status().is_success() {
        // return Err(Box::new(std::io::Error::new(
        //     std::io::ErrorKind::Other,
        //     format!("Error downloading part: {}", part_url),
        // )));
        return None;
        // return Err(format!(
        //     "Error downloading part: status_code: {}, part: {}",
        //     res.status(),
        //     part_url
        // )
        // .into());
    }

    let mut file = File::create(&path).await.ok()?;
    while let Some(chunk) = res.chunk().await.ok()? {
        file.write_all(&chunk).await.ok()?;
    }

    Some(path)
}

//endregion

pub async fn get_client<'a>() -> Result<TwitchClient<'a>> {
    trace!("get_client");
    let conf: Conf = Config::builder()
        .env()
        .file("./settings.toml")
        .file(shellexpand::tilde("~/twba/config.toml").into_owned())
        .file(std::env::var("TWBA_CONFIG").unwrap_or_else(|_| "~/twba/config.toml".to_string()))
        .load()
        .expect("Failed to load config");
    info!("get_client: config: {:?}", conf);
    let client_id = ClientId::new(conf.twitch.client_id.clone());
    let client_secret = ClientSecret::new(conf.twitch.client_secret.clone());
    info!("creating TwitchClient");

    let client = TwitchClient::new(client_id, client_secret, conf).await?;
    Ok(client)
}

//region static functions

pub fn sort_video_part_filenames(video_id: &str, files: &mut Vec<PathBuf>) {
    files.sort_by_key(|f| {
        let number = f
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .replace("-muted", "") //remove the muted for the sort if its there
            .replace("-unmuted", "") //remove the muted for the sort if its there
            .replace(".ts", "") //remove the file ending for the sort
            ;

        match number.parse::<u32>() {
            Ok(n) => n,
            Err(e) => {
                warn!(
                    "potentially catchable error while parsing the file number: {}\n{}",
                    number, e
                );
                if !number.starts_with(&format!("{}v", video_id)) || !number.contains("-") {
                    panic!("Error while parsing the file number: {}", number)
                }
                let number = number.split("-").collect::<Vec<&str>>()[1];
                number
                    .parse()
                    .expect(format!("Error while parsing the file number: {}", number).as_str())
            }
        }
    });
}
pub async fn convert_ts_to_mp4(video_mp4: &PathBuf, video_ts: &PathBuf) -> Result<()> {
    //convert to mp4
    info!("converting to mp4");
    if video_mp4.exists() {
        std::fs::remove_file(&video_mp4)?;
    }
    debug!(
        "running ffmpeg command: ffmpeg -i {} -c copy {}",
        video_ts.display(),
        video_mp4.display()
    );
    let mut cmd = Command::new("ffmpeg");
    let convert_start_time = Instant::now();
    cmd.arg("-i")
        .arg(&video_ts)
        .arg("-c")
        .arg("copy")
        .arg(&video_mp4);
    let result = cmd.output().await;
    //stop the time how long it takes to convert
    let duration = Instant::now().duration_since(convert_start_time);
    if let Err(e) = result {
        return Err(Box::new(DownloadError::Ffmpeg(duration, e.into())));
    }
    debug!("ffmpeg command finished");
    info!("duration: {:?}", duration);
    Ok(())
}

pub async fn combine_parts_into_single_ts(files: Vec<PathBuf>, video_ts: &PathBuf) -> Result<()> {
    debug!("combining all parts of video");
    debug!("part amount: {}", files.len());
    let mut video_ts_file = File::create(&video_ts).await?;
    for file_path in &files {
        debug!("{:?}", file_path.file_name());
        let mut file = File::open(&file_path).await?;
        tokio::io::copy(&mut file, &mut video_ts_file).await?;
        tokio::fs::remove_file(&file_path).await?;
    }
    Ok(())
}

fn get_base_url(url: &str) -> String {
    let mut base_url = url.to_string();
    let mut i = base_url.len() - 1;
    while i > 0 {
        if base_url.chars().nth(i).unwrap() == '/' {
            break;
        }
        i -= 1;
    }
    base_url.truncate(i + 1);
    base_url
}

fn convert_twitch_timestamp(time: Timestamp) -> DateTime<Utc> {
    let time1: String = time.take() as String;
    trace!("convert_twitch_timestamp: {}", time1);
    convert_twitch_time(&time1)
}

/// parse a duration from a string like '2h49m47s'
fn convert_twitch_duration(duration: &str) -> chrono::Duration {
    trace!("convert_twitch_duration: {}", duration);

    #[allow(clippy::collapsible_str_replace)]
    let duration = duration
        .replace('h', ":")
        .replace('m', ":")
        .replace('s', "");

    let mut t = duration.split(':').collect::<Vec<&str>>();

    let secs_str = t.pop();
    let secs: i64 = match secs_str {
        Some(secs) => secs
            .parse()
            .expect(format!("Failed to parse secs: {:?}", secs).as_str()),
        None => 0,
    };

    let mins_str = t.pop();
    let mins: i64 = match mins_str {
        Some(mins) => mins
            .parse()
            .expect(format!("Failed to parse mins: {:?}", mins).as_str()),
        None => 0,
    };

    let hours_str = t.pop();
    let hours: i64 = match hours_str {
        Some(hours) => hours
            .parse()
            .expect(format!("Failed to parse hours: {:?}", hours).as_str()),
        None => 0,
    };

    debug!("hours: {}, mins: {}, secs: {}", hours, mins, secs);
    let millis = /* millis +*/ secs * 1000 + mins * 60 * 1000 + hours * 60 * 60 * 1000;

    let res = chrono::Duration::milliseconds(millis);
    res
}

fn convert_twitch_time(time: &str) -> DateTime<Utc> {
    return convert_twitch_time_info(time.to_string(), "%Y-%m-%dT%H:%M:%S%z");
}

fn convert_twitch_time_info(res: String, fmt: &str) -> DateTime<Utc> {
    trace!(
        "convert_twitch_time: time: '{}' with format: '{}'",
        res,
        fmt
    );
    let mut res = res;
    if res.ends_with("Z") {
        res = format!("{}+00:00", res.strip_suffix("Z").unwrap());
    } else if !res.contains("+") {
        res = format!("{}{}", res, "+00:00");
    }

    debug!("convert_twitch_time: time: {} with format: {}", res, fmt);
    let res = chrono::DateTime::parse_from_str(&res, fmt)
        .expect(format!("Failed to parse time: {}", res).as_str());
    res.with_timezone(&Utc)
}

//endregion

//region Twitch access token structs (maybe find out how to extract those values directly without these structs)
#[derive(Debug, Deserialize, Serialize)]
pub struct TwitchVideoAccessTokenResponse {
    pub data: VideoAccessTokenResponseData,
}

//noinspection ALL
#[derive(Debug, Deserialize, Serialize)]
pub struct VideoAccessTokenResponseData {
    #[serde(rename = "videoPlaybackAccessToken")]
    pub video_playback_access_token: VideoAccessTokenResponseDataAccessToken,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct VideoAccessTokenResponseDataAccessToken {
    pub value: String,
    pub signature: String,
}
//endregion

//region tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_twitch_time() {
        let time = "2021-01-01T00:00:00Z";
        let time = convert_twitch_time(time);
        assert_eq!(time.to_string(), "2021-01-01 00:00:00 UTC");
    }

    #[test]
    fn test_convert_twitch_time_info() {
        let time = "2021-01-01T00:00:00+00:00";
        let time = convert_twitch_time_info(time.to_string(), "%Y-%m-%dT%H:%M:%S%z");
        assert_eq!(time.to_string(), "2021-01-01 00:00:00 UTC");
        let time = "2021-01-01T00:00:00Z";
        let time = convert_twitch_time_info(time.to_string(), "%Y-%m-%dT%H:%M:%S%z");
        assert_eq!(time.to_string(), "2021-01-01 00:00:00 UTC");
    }

    #[test]
    fn test_convert_twitch_duration() {
        let duration = "2h49m47s";
        println!("duration: {:?}", duration);
        let duration = convert_twitch_duration(duration);
        println!("duration: {:?}", duration);
        assert_eq!(duration.num_seconds(), 10187);
    }
    #[test]
    fn test_convert_twitch_duration2() {
        let duration = "3h56m57s";
        println!("duration: {:?}", duration);
        let duration = convert_twitch_duration(duration);
        println!("duration: {:?}", duration);
        assert_eq!(duration.num_seconds() as i32, 3 * 3600 + 56 * 60 + 57);
    }

    #[test]
    fn test_get_base_url() {
        let url = "https://dqrpb9wgowsf5.cloudfront.net/5f3ee3729979d8e1eab3_halfwayhardcore_42051664507_1680818535/chunked/index-dvr.m3u8";
        let base_url = get_base_url(url);
        assert_eq!(base_url, "https://dqrpb9wgowsf5.cloudfront.net/5f3ee3729979d8e1eab3_halfwayhardcore_42051664507_1680818535/chunked/");
    }
}

//endregion
