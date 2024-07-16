use derive_more::Deref;
use dotenvy::{dotenv, var};
use eframe::{App, NativeOptions};
use egui::{
  Align, Button, CentralPanel, Color32, Image, Label, Layout, Rgba, RichText, ScrollArea, TextEdit,
  Vec2,
};
use egui_video::{AudioDevice, Player};
use google_youtube3::{
  api::{
    ChannelSnippet, PlaylistItem, PlaylistItemListResponse, PlaylistItemSnippet, PlaylistSnippet,
  },
  hyper::{self, client::HttpConnector},
  hyper_rustls::{self, HttpsConnector},
  oauth2::{ApplicationSecret, InstalledFlowAuthenticator, InstalledFlowReturnMethod},
  YouTube,
};
use std::{
  path::PathBuf,
  sync::{
    mpsc::{channel, Receiver, Sender},
    Arc,
  },
};

#[tokio::main]
async fn main() {
  dotenv().expect("no .env found");

  _ = eframe::run_native(
    "YouTube Playlist Player",
    NativeOptions::default(),
    Box::new(move |ctx| {
      egui_extras::install_image_loaders(&ctx.egui_ctx);

      let (emit_yt_client, listen_yt_client) = channel::<YouTubeClient>();
      let (emit_playlist_info, listen_playlist_info) = channel::<PlaylistInfo>();
      let (emit_playlist_videos_info, listen_playlist_videos_info) = channel::<PlaylistVideos>();
      let (emit_downloaded_path, listen_downloaded_path) = channel::<PathBuf>();
      let (emit_download_status, listen_download_status) = channel::<DownloadStatus>();

      let cloned_yt_emit = emit_yt_client.clone();
      tokio::spawn(async move { cloned_yt_emit.send(Visualizer::fetch_youtube_client().await) });

      Ok(Box::new(Visualizer {
        current_playlist_id: String::new(),
        current_page_cursor: None,

        current_downloaded_path: None,

        yt_client: None,
        playlist_info: None,
        playlist_videos_info: None,

        tasks: Tasks {
          listen_yt_client,
          emit_playlist_info,
          listen_playlist_info,
          emit_playlist_videos_info,
          listen_playlist_videos_info,
          emit_downloaded_path,
          listen_downloaded_path,
          emit_download_status,
          listen_download_status,
        },

        download_status: DownloadStatus::Idle,

        current_watching_path: None,

        video_player: None,
        audio_device: AudioDevice::new().expect("failed to create audio device"),
      }))
    }),
  );
}

#[derive(Deref)]
struct YouTubeClient(YouTube<HttpsConnector<HttpConnector>>);

#[derive(Default, PartialEq)]
enum DownloadStatus {
  #[default]
  Idle,
  Pending,
  Downloading,
  Finished,
  Failed,
}

struct Tasks {
  listen_yt_client: Receiver<YouTubeClient>,

  emit_playlist_info: Sender<PlaylistInfo>,
  listen_playlist_info: Receiver<PlaylistInfo>,

  emit_downloaded_path: Sender<PathBuf>,
  listen_downloaded_path: Receiver<PathBuf>,

  emit_playlist_videos_info: Sender<PlaylistVideos>,
  listen_playlist_videos_info: Receiver<PlaylistVideos>,

  emit_download_status: Sender<DownloadStatus>,
  listen_download_status: Receiver<DownloadStatus>,
}

struct Visualizer {
  current_playlist_id: String,
  current_page_cursor: Option<String>,

  current_downloaded_path: Option<PathBuf>,

  yt_client: Option<Arc<YouTubeClient>>,
  playlist_info: Option<PlaylistInfo>,
  playlist_videos_info: Option<PlaylistVideos>,

  tasks: Tasks,

  download_status: DownloadStatus,

  current_watching_path: Option<PathBuf>,

  video_player: Option<Player>,
  audio_device: AudioDevice,
}

impl App for Visualizer {
  fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
    if let Ok(yt_client) = self.tasks.listen_yt_client.try_recv() {
      self.yt_client = Some(Arc::new(yt_client));
    }

    if let Ok(playlist_info) = self.tasks.listen_playlist_info.try_recv() {
      self.playlist_info = Some(playlist_info);
    }

    if let Ok(playlist_videos_info) = self.tasks.listen_playlist_videos_info.try_recv() {
      self.playlist_videos_info = Some(playlist_videos_info);
    }

    if let Ok(download_status) = self.tasks.listen_download_status.try_recv() {
      if download_status == DownloadStatus::Finished {
        self.download_status = DownloadStatus::Idle;
      } else {
        self.download_status = download_status;
      }
    }

    if let Ok(downloaded_path) = self.tasks.listen_downloaded_path.try_recv() {
      if self.current_watching_path.is_none() {
        if let Ok(video_player) = Player::new(ctx, &downloaded_path.to_string_lossy().to_string()) {
          self.video_player = Some(video_player);
          self.current_watching_path = Some(downloaded_path.clone());
        }
      }

      self.current_downloaded_path = Some(downloaded_path);
    }

    CentralPanel::default().show(ctx, |ui| {
      ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
        ui.label("YouTube Playlist ID:");
        ui.add(TextEdit::singleline(&mut self.current_playlist_id));

        if ui.button("🔍").clicked() {
          let cloned_playlist_info_emit = self.tasks.emit_playlist_info.clone();
          let cloned_playlist_videos_info_emit = self.tasks.emit_playlist_videos_info.clone();
          let Some(yt_client) = &self.yt_client else {
            return;
          };

          let cloned_yt_client = yt_client.clone();
          let cloned_playlist_id = self.current_playlist_id.clone();
          let cloned_cursor = self.current_page_cursor.clone();

          tokio::spawn(async move {
            if let Some(playlist_info) =
              Self::fetch_playlist_info(cloned_yt_client.clone(), &cloned_playlist_id).await
            {
              _ = cloned_playlist_info_emit.send(playlist_info);
            }

            if let Some(playlist_videos_info) = Self::fetch_video_page_with_cursor(
              cloned_yt_client.clone(),
              &cloned_playlist_id,
              cloned_cursor,
            )
            .await
            {
              _ = cloned_playlist_videos_info_emit.send(playlist_videos_info);
            }
          });
        }
      });

      ScrollArea::vertical().show(ui, |ui| {
        match self.download_status {
          DownloadStatus::Downloading => {
            ui.label("downloading video...");
          }
          DownloadStatus::Failed => {
            ui.label("download failed");
          }
          _ => {}
        }

        if self.video_player.is_some() && ui.button("back").clicked() {
          self.current_watching_path = None;
          self.video_player = None;
          return;
        }

        if let Some(video_player) = self.video_player.as_mut() {
          video_player.ui(ui, video_player.size);
          return;
        }

        if let Some(playlist_info) = &self.playlist_info {
          ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
            ui.add(
              Image::from_uri(&playlist_info.channel.avatar_url).max_size(Vec2::new(40.0, 40.0)),
            );
            ui.with_layout(Layout::top_down(Align::TOP), |ui| {
              ui.label(RichText::new(&playlist_info.title).size(18.0));
              ui.with_layout(Layout::left_to_right(Align::TOP), |ui| {
                ui.label("by");
                ui.hyperlink_to(
                  &playlist_info.channel.name,
                  format!("https://youtube.com/channel/{}", &playlist_info.channel.id),
                );
              });
            });
          });
        }

        ui.separator();

        if let Some(playlist_videos_info) = &self.playlist_videos_info {
          ui.with_layout(Layout::right_to_left(Align::TOP), |ui| {
            if ui
              .add(
                Button::new(RichText::new("download all videos").color(Color32::WHITE))
                  .fill(Rgba::from_rgb(0.0, 0.25, 0.40)),
              )
              .clicked()
            {
              let cloned_download_status_emit = self.tasks.emit_download_status.clone();

              _ = cloned_download_status_emit
                .clone()
                .send(DownloadStatus::Pending);

              let id_path_map = playlist_videos_info
                .videos
                .iter()
                .filter_map(|PlaylistVideo { id, .. }| {
                  let path = PathBuf::from(format!(
                    concat!(env!("CARGO_MANIFEST_DIR"), "/youtube/{}.mp4"),
                    id
                  ));

                  (!path.exists()).then_some((id, path))
                })
                .map(move |(id, path)| {
                  let id = id.clone();

                  async move {
                    let options = rusty_ytdl::VideoOptions {
                      quality: rusty_ytdl::VideoQuality::Lowest,
                      filter: rusty_ytdl::VideoSearchOptions::VideoAudio,
                      ..Default::default()
                    };

                    let video = rusty_ytdl::Video::new_with_options(
                      format!("https://youtube.com/watch?v={id}"),
                      options,
                    )
                    .expect("failed to create video downloader");

                    if let Some(parent) = path.parent() {
                      _ = std::fs::create_dir_all(parent);
                    }

                    _ = std::fs::write(&path, b"");
                    _ = video.download(&path).await;
                  }
                })
                .collect::<Vec<_>>();

              tokio::spawn(async move {
                _ = cloned_download_status_emit.send(DownloadStatus::Downloading);
                futures_util::future::join_all(id_path_map).await;
                _ = cloned_download_status_emit.send(DownloadStatus::Finished);
              });
            }
          });
          ui.with_layout(
            Layout::left_to_right(Align::TOP).with_main_wrap(true),
            |ui| {
              for video in playlist_videos_info.videos.iter() {
                ui.with_layout(Layout::top_down(Align::TOP).with_main_wrap(true), |ui| {
                  ui.add(Image::from_uri(&video.thumbnail_url).max_width(200.0));

                  ui.add_sized([200.0, 32.0], Label::new(&video.title).wrap());

                  if ui.button("watch").clicked() {
                    let id = video.id.clone();

                    let path = PathBuf::from(format!(
                      concat!(env!("CARGO_MANIFEST_DIR"), "/youtube/{}.mp4"),
                      id
                    ));

                    if path.exists() {
                      _ = self.tasks.emit_downloaded_path.send(path);
                    } else {
                      let cloned_downloaded_path_emit = self.tasks.emit_downloaded_path.clone();
                      let cloned_download_status_emit = self.tasks.emit_download_status.clone();

                      tokio::spawn(async move {
                        _ = cloned_download_status_emit.send(DownloadStatus::Pending);

                        let options = rusty_ytdl::VideoOptions {
                          quality: rusty_ytdl::VideoQuality::Lowest,
                          filter: rusty_ytdl::VideoSearchOptions::VideoAudio,
                          ..Default::default()
                        };

                        let video = rusty_ytdl::Video::new_with_options(
                          format!("https://youtube.com/watch?v={id}"),
                          options,
                        )
                        .expect("failed to create video downloader");

                        if let Some(parent) = path.parent() {
                          _ = std::fs::create_dir_all(parent);
                        }

                        _ = std::fs::write(&path, b"");
                        _ = cloned_download_status_emit.send(DownloadStatus::Downloading);

                        if video.download(&path).await.is_ok() {
                          _ = cloned_downloaded_path_emit.send(path);
                          _ = cloned_download_status_emit.send(DownloadStatus::Finished);
                        }
                      });
                    }
                  }
                });
              }
            },
          );
        } else {
          ui.label("Enter a YouTube playlist ID in the textbox above and click the search button");
        }
      });
    });
  }
}

struct YouTubeChannel {
  id: String,
  name: String,
  avatar_url: String,
}

struct PlaylistInfo {
  id: String,
  title: String,
  channel: YouTubeChannel,
}

struct PlaylistVideo {
  id: String,
  title: String,
  thumbnail_url: String,
}

struct PlaylistVideos {
  videos: Vec<PlaylistVideo>,
  next_cursor: Option<String>,
}

impl Visualizer {
  async fn fetch_youtube_client() -> YouTubeClient {
    let secret = ApplicationSecret {
      client_id: var("CLIENT_ID").expect("no CLIENT_ID env var found"),
      client_secret: var("CLIENT_SECRET").expect("no CLIENT_SECRET env var found"),
      auth_uri: var("AUTH_URI").expect("no AUTH_URI env var found"),
      token_uri: var("TOKEN_URI").expect("no TOKEN_URI env var found"),
      redirect_uris: vec!["http://localhost:6969".into()],
      project_id: None,
      client_email: None,
      auth_provider_x509_cert_url: None,
      client_x509_cert_url: None,
    };

    let auth = InstalledFlowAuthenticator::builder(
      secret,
      InstalledFlowReturnMethod::HTTPPortRedirect(6969),
    )
    .build()
    .await
    .expect("failed to authenticate");

    YouTubeClient(YouTube::new(
      hyper::Client::builder().build(
        hyper_rustls::HttpsConnectorBuilder::new()
          .with_native_roots()
          .unwrap()
          .https_or_http()
          .enable_http1()
          .build(),
      ),
      auth,
    ))
  }

  async fn fetch_channel(yt_client: Arc<YouTubeClient>, user_id: &str) -> Option<YouTubeChannel> {
    let (_, channels) = yt_client
      .channels()
      .list(&vec!["snippet".into(), "contentDetails".into()])
      .add_id(user_id)
      .doit()
      .await
      .ok()?;

    channels.items?.into_iter().next().and_then(|channel| {
      let ChannelSnippet {
        title, thumbnails, ..
      } = channel.snippet?;

      Some(YouTubeChannel {
        id: user_id.to_string(),
        name: title?,
        avatar_url: thumbnails?.default?.url?,
      })
    })
  }

  async fn fetch_playlist_info(
    yt_client: Arc<YouTubeClient>,
    playlist_id: &str,
  ) -> Option<PlaylistInfo> {
    let (_, playlists) = yt_client
      .playlists()
      .list(&vec!["snippet".into()])
      .add_id(playlist_id)
      .doit()
      .await
      .ok()?;

    let PlaylistSnippet {
      channel_id, title, ..
    } = playlists.items?.into_iter().next()?.snippet?;

    Some(PlaylistInfo {
      id: playlist_id.to_string(),
      title: title?,
      channel: Self::fetch_channel(yt_client, &channel_id?).await?,
    })
  }

  async fn fetch_video_page_with_cursor(
    yt_client: Arc<YouTubeClient>,
    playlist_id: &str,
    cursor: Option<String>,
  ) -> Option<PlaylistVideos> {
    let mut videos_query = yt_client
      .playlist_items()
      .list(&vec!["snippet".into(), "contentDetails".into()])
      .playlist_id(playlist_id);

    if let Some(cursor) = cursor {
      videos_query = videos_query.page_token(&cursor);
    }

    let (_, videos) = videos_query.doit().await.ok()?;

    let PlaylistItemListResponse {
      items: videos,
      next_page_token: next_cursor,
      ..
    } = videos;

    Some(PlaylistVideos {
      videos: videos?
        .into_iter()
        .filter_map(
          |PlaylistItem {
             snippet,
             content_details,
             ..
           }| {
            let PlaylistItemSnippet {
              title, thumbnails, ..
            } = snippet?;

            Some(PlaylistVideo {
              id: content_details?.video_id?,
              title: title?,
              thumbnail_url: thumbnails?.default?.url?,
            })
          },
        )
        .collect::<Vec<_>>(),
      next_cursor,
    })
  }
}
