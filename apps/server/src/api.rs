//! The domain API. Nine nouns, not seventy-two commands.
//!
//! Every handler here takes a [`Caller`] and passes its id to a `*_owned`
//! database call, which filters on `user_id` in SQL. Knowing another user's
//! broadcast id therefore buys nothing: the row simply is not found.

use crate::auth::Caller;
use crate::error::ApiError;
use crate::state::App;
use axum::extract::{Multipart, Path, Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::Json;
use louver_cloud::entitlement::MAX_UPLOAD_BYTES;
use louver_cloud::{Broadcast, BroadcastEvent, CloudError, CloudMedia, StreamDestination};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::io::Write;
use std::time::Duration;

type Out<T> = std::result::Result<Json<T>, ApiError>;

// --- media ----------------------------------------------------------------

pub async fn list_media(State(app): State<App>, Caller(uid): Caller) -> Out<Vec<CloudMedia>> {
    Ok(Json(crate::blocking(move || app.db.media_for(&uid)).await?))
}

pub async fn get_media(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> Out<CloudMedia> {
    Ok(Json(crate::blocking(move || app.db.media_owned(&uid, &id)).await?))
}

/// Upload a video.
///
/// The body is streamed to a temp file and abandoned the moment it passes the
/// plan's per-file limit, so a caller cannot fill the disk by ignoring it. The
/// response comes back as soon as the row exists; analysis and preparation run
/// on their own thread, exactly as the desktop's add does.
pub async fn upload_media(
    State(app): State<App>,
    Caller(uid): Caller,
    mut form: Multipart,
) -> Out<CloudMedia> {
    let ceiling = {
        let db = app.db.clone();
        let uid = uid.clone();
        crate::blocking(move || db.limit(&uid, MAX_UPLOAD_BYTES)).await?
    };

    let mut saved: Option<(String, std::path::PathBuf)> = None;
    while let Some(mut field) =
        form.next_field().await.map_err(|e| CloudError::Invalid(format!("업로드를 읽을 수 없습니다: {e}")))?
    {
        if field.name() != Some("file") {
            continue;
        }
        let filename = field
            .file_name()
            .map(str::to_string)
            .ok_or_else(|| CloudError::Invalid("파일 이름이 없습니다".into()))?;

        let temp = app.upload_tmp.join(format!("{}.part", louver_cloud::new_id()));
        let mut file = std::fs::File::create(&temp)?;
        let mut written: i64 = 0;
        loop {
            let chunk = match field.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    drop(file);
                    let _ = std::fs::remove_file(&temp);
                    return Err(CloudError::Invalid(format!("업로드가 중단되었습니다: {e}")).into());
                }
            };
            written += chunk.len() as i64;
            if written > ceiling {
                drop(file);
                let _ = std::fs::remove_file(&temp);
                return Err(CloudError::LimitReached {
                    limit: MAX_UPLOAD_BYTES,
                    used: written,
                    allowed: ceiling,
                }
                .into());
            }
            file.write_all(&chunk)?;
        }
        file.flush()?;
        saved = Some((filename, temp));
        break;
    }

    let (filename, temp) = saved.ok_or_else(|| CloudError::Invalid("파일이 없습니다".into()))?;
    let ingest = app.ingest.clone();
    let result = crate::blocking(move || {
        let r = ingest.accept_upload(&uid, &filename, &temp);
        // The temp file has been copied into storage, or refused. Either way it
        // is ours to clean up.
        let _ = std::fs::remove_file(&temp);
        r
    })
    .await?;
    Ok(Json(result))
}

pub async fn delete_media(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> std::result::Result<Json<Gone>, ApiError> {
    crate::blocking(move || {
        let m = app.db.delete_media_owned(&uid, &id)?;
        let _ = app.storage.delete(&m.storage_path);
        if let Some(p) = &m.prepared_path {
            let _ = app.storage.delete(p);
        }
        Ok(())
    })
    .await?;
    Ok(Json(Gone { deleted: true }))
}

#[derive(Serialize)]
pub struct Gone {
    pub deleted: bool,
}

// --- stream destinations --------------------------------------------------

#[derive(Deserialize)]
pub struct NewDestination {
    pub label: String,
    pub rtmps_url: String,
    /// Seen once, here, on its way to the sealed store. It is never read back
    /// out to any response.
    pub stream_key: String,
}

pub async fn list_destinations(State(app): State<App>, Caller(uid): Caller) -> Out<Vec<StreamDestination>> {
    Ok(Json(crate::blocking(move || app.db.destinations_for(&uid)).await?))
}

/// Save a YouTube ingest target.
///
/// The key goes straight into the sealed credential store under
/// `destination:<id>`; the row keeps only a mask. The response is built from the
/// row, so there is no code path that could return the key.
pub async fn create_destination(
    State(app): State<App>,
    Caller(uid): Caller,
    Json(body): Json<NewDestination>,
) -> Out<StreamDestination> {
    let made = crate::blocking(move || {
        let key = body.stream_key.trim().to_string();
        if key.is_empty() {
            return Err(CloudError::Invalid("스트림 키를 입력해 주세요".into()));
        }
        if !body.rtmps_url.starts_with("rtmps://") && !body.rtmps_url.starts_with("rtmp://") {
            return Err(CloudError::Invalid("RTMPS 주소를 확인해 주세요".into()));
        }
        let dest = app.db.create_destination(&uid, body.label.trim(), body.rtmps_url.trim(), MASK)?;
        let account = louver_cloud::credentials::destination_account(&dest.id);
        if let Err(e) = app.keys.set(&account, &key) {
            // Never leave a destination whose key is missing: a broadcast would
            // fail later with no way for the user to see why.
            let _ = app.db.delete_destination_owned(&uid, &dest.id);
            return Err(CloudError::Engine(e.message));
        }
        Ok(dest)
    })
    .await?;
    Ok(Json(made))
}

pub async fn delete_destination(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> std::result::Result<Json<Gone>, ApiError> {
    crate::blocking(move || {
        app.db.delete_destination_owned(&uid, &id)?;
        let _ = app.keys.delete(&louver_cloud::credentials::destination_account(&id));
        Ok(())
    })
    .await?;
    Ok(Json(Gone { deleted: true }))
}

/// What a browser is ever shown in place of a stream key.
const MASK: &str = "••••••••••••";

// --- broadcasts -----------------------------------------------------------

#[derive(Deserialize)]
pub struct NewBroadcast {
    pub name: String,
    /// A pasted-key destination. Empty when `youtube_account_id` is given,
    /// because YouTube has not made one yet.
    #[serde(default)]
    pub destination_id: String,
    /// A connected channel, for a broadcast 247streams will create on YouTube
    /// itself. §5: the two providers are chosen here and nowhere else.
    #[serde(default)]
    pub youtube_account_id: Option<String>,
    /// The playlist, in order. This is what the web app sends.
    #[serde(default)]
    pub items: Vec<louver_cloud::db::NewItem>,
    /// Shorthand for a playlist of plain videos.
    #[serde(default)]
    pub media_ids: Vec<String>,
    /// One video, as the first release's clients sent it. Still accepted so
    /// that a script written against that API keeps working.
    #[serde(default)]
    pub media_id: Option<String>,
    #[serde(default = "yes")]
    pub loop_forever: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub tags: Option<String>,
    #[serde(default)]
    pub category: Option<String>,
    #[serde(default)]
    pub privacy: Option<louver_cloud::Privacy>,
    #[serde(default)]
    pub settings: Option<louver_cloud::StreamSettings>,
    #[serde(default)]
    pub schedule: Option<louver_cloud::Schedule>,
}

impl NewBroadcast {
    /// One playlist out of the three shapes a client may send.
    fn playlist(&self) -> Vec<louver_cloud::db::NewItem> {
        if !self.items.is_empty() {
            return self.items.clone();
        }
        let ids = if !self.media_ids.is_empty() {
            self.media_ids.clone()
        } else {
            self.media_id.clone().into_iter().collect()
        };
        ids.into_iter()
            .map(|media_id| louver_cloud::db::NewItem { media_id, enabled: true, repeat_count: 1 })
            .collect()
    }
}

fn yes() -> bool {
    true
}

pub async fn list_broadcasts(
    State(app): State<App>,
    Caller(uid): Caller,
) -> Out<louver_cloud::manager::Dashboard> {
    Ok(Json(crate::blocking(move || app.mgr.dashboard(&uid)).await?))
}

/// Create a broadcast with its playlist, metadata, settings and schedule.
///
/// The playlist is written in the same request rather than left for a second
/// call: a broadcast with no videos cannot be started, and leaving one in the
/// database would mean a dashboard row that fails the moment it is used.
pub async fn create_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Json(b): Json<NewBroadcast>,
) -> Out<louver_cloud::BroadcastDetail> {
    let made = crate::blocking(move || {
        let playlist = b.playlist();
        let first = playlist
            .first()
            .ok_or_else(|| CloudError::Invalid("영상을 최소 한 개 선택해 주세요".into()))?
            .media_id
            .clone();

        // §5: one of two providers. A connected account has no destination yet —
        // YouTube cannot be asked for an ingestion address before there is a
        // broadcast to bind it to — so an empty row of the right kind is
        // reserved and filled in by `provision` below.
        let account = b.youtube_account_id.clone().filter(|a| !a.trim().is_empty());
        // Asked for before anything is written, so a server with no Google
        // credentials refuses the request instead of leaving a broadcast behind
        // that could never be provisioned.
        let provider = match &account {
            Some(_) => Some(app.youtube.clone().ok_or_else(|| {
                CloudError::Invalid("이 서버에는 YouTube 연결이 설정되지 않았습니다.".into())
            })?),
            None => None,
        };
        let reserved = match &account {
            Some(account_id) => Some(app.db.reserve_youtube_destination(&uid, account_id)?),
            None => None,
        };
        let destination_id = match &reserved {
            Some(d) => d.id.clone(),
            None => b.destination_id.trim().to_string(),
        };
        if destination_id.is_empty() {
            return Err(CloudError::Invalid("송출 대상을 선택해 주세요".into()));
        }

        let made = app.db.create_broadcast(&uid, b.name.trim(), &first, &destination_id, b.loop_forever)?;
        let items = app.db.replace_items(&uid, &made.id, &playlist)?;
        let patch = louver_cloud::BroadcastPatch {
            title: Some(b.title.clone().unwrap_or_else(|| b.name.trim().to_string())),
            description: b.description.clone(),
            tags: b.tags.clone(),
            category: b.category.clone(),
            privacy: b.privacy,
            settings: b.settings.clone(),
            schedule: b.schedule.clone(),
            ..Default::default()
        };
        let broadcast = app.db.update_broadcast_owned(&uid, &made.id, &patch)?;

        if let (Some(account_id), Some(yt)) = (&account, &provider) {
            // The metadata is written first, on purpose: `provision` sends the
            // title, description and privacy this broadcast now has.
            if let Err(e) = yt.provision(&uid, &made.id, account_id) {
                // Nothing half-made is left behind. A broadcast pointed at an
                // address YouTube never gave us could not be started, and the
                // row would only be there to confuse whoever found it.
                let _ = app.db.delete_broadcast_owned(&uid, &made.id);
                if let Some(d) = &reserved {
                    let _ = app.db.delete_destination_owned(&uid, &d.id);
                }
                return Err(e);
            }
            let broadcast = app.db.broadcast_owned(&uid, &made.id)?;
            return Ok(louver_cloud::BroadcastDetail { broadcast, items });
        }

        Ok(louver_cloud::BroadcastDetail { broadcast, items })
    })
    .await?;
    Ok(Json(made))
}

/// Change a broadcast. Anything absent is left as it was.
pub async fn update_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
    Json(patch): Json<louver_cloud::BroadcastPatch>,
) -> Out<Broadcast> {
    Ok(Json(
        crate::blocking(move || {
            let broadcast = app.db.update_broadcast_owned(&uid, &id, &patch)?;
            // §10: a title changed here is a title changed on YouTube. Only for
            // a connected account — a pasted key cannot, and `sync_metadata`
            // returns `Ok` for one rather than making the caller ask.
            //
            // A refusal from YouTube does not fail the save. The edit is already
            // stored, so answering with an error would tell the user their change
            // was lost when it was not; YouTube also refuses some fields while a
            // broadcast is live, which is a thing to be told rather than a thing
            // to lose work over. `sync_metadata` records the reason against the
            // broadcast, and the dashboard shows it as "YouTube · …", so the
            // reply carries the row as it now reads.
            let Some(yt) = &app.youtube else { return Ok(broadcast) };
            match yt.sync_metadata(&uid, &id) {
                Ok(()) => Ok(broadcast),
                Err(_) => app.db.broadcast_owned(&uid, &id),
            }
        })
        .await?,
    ))
}

/// The playlist, in order.
pub async fn list_items(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> Out<Vec<louver_cloud::BroadcastItem>> {
    Ok(Json(crate::blocking(move || app.db.items_owned(&uid, &id)).await?))
}

/// Replace the playlist with this list, in this order.
///
/// The whole list, because that is what a drag produces: every position after
/// the moved row changed, and sending the result makes the stored order and the
/// drawn order the same thing.
pub async fn replace_items(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
    Json(items): Json<Vec<louver_cloud::db::NewItem>>,
) -> Out<Vec<louver_cloud::BroadcastItem>> {
    Ok(Json(crate::blocking(move || app.db.replace_items(&uid, &id, &items)).await?))
}

pub async fn get_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> Out<louver_cloud::BroadcastDetail> {
    Ok(Json(
        crate::blocking(move || {
            Ok(louver_cloud::BroadcastDetail {
                broadcast: app.db.broadcast_owned(&uid, &id)?,
                items: app.db.items_for(&id)?,
            })
        })
        .await?,
    ))
}

pub async fn start_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> Out<Broadcast> {
    Ok(Json(
        crate::blocking(move || {
            app.mgr.start(&uid, &id)?;
            app.db.broadcast_owned(&uid, &id)
        })
        .await?,
    ))
}

pub async fn stop_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> Out<Broadcast> {
    Ok(Json(
        crate::blocking(move || {
            app.mgr.stop(&uid, &id)?;
            app.db.broadcast_owned(&uid, &id)
        })
        .await?,
    ))
}

pub async fn restart_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> Out<Broadcast> {
    Ok(Json(
        crate::blocking(move || {
            app.mgr.restart(&uid, &id)?;
            app.db.broadcast_owned(&uid, &id)
        })
        .await?,
    ))
}

pub async fn delete_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> std::result::Result<Json<Gone>, ApiError> {
    crate::blocking(move || {
        // Stopping first is what makes the delete safe: the row cannot vanish
        // while a worker still holds its slot.
        let _ = app.mgr.stop(&uid, &id);
        app.db.delete_broadcast_owned(&uid, &id)
    })
    .await?;
    Ok(Json(Gone { deleted: true }))
}

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub limit: Option<i64>,
}

pub async fn broadcast_logs(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
    Query(q): Query<LogQuery>,
) -> Out<Vec<BroadcastEvent>> {
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    Ok(Json(crate::blocking(move || app.db.events_owned(&uid, &id, limit)).await?))
}

// --- live updates ---------------------------------------------------------

/// The dashboard as it changes, over one long-lived response.
///
/// SSE rather than a socket: the traffic is one-way, it survives proxies that
/// know nothing of upgrades, and a dropped connection reconnects by itself. The
/// stream is scoped to the caller, so it cannot become a way to watch someone
/// else's broadcasts.
pub async fn events(
    State(app): State<App>,
    Caller(uid): Caller,
) -> Sse<impl tokio_stream::Stream<Item = std::result::Result<Event, Infallible>>> {
    use tokio_stream::StreamExt;

    let ticks = tokio_stream::wrappers::IntervalStream::new(tokio::time::interval(Duration::from_secs(2)));
    let stream = ticks.then(move |_| {
        let app = app.clone();
        let uid = uid.clone();
        async move {
            let snapshot = crate::blocking(move || app.mgr.dashboard(&uid)).await;
            let event = match snapshot {
                Ok(d) => Event::default()
                    .event("dashboard")
                    .json_data(d)
                    .unwrap_or_else(|_| Event::default().event("error").data("직렬화 실패")),
                // A failure here is the caller's session ending, not something
                // to spell out over a public channel.
                Err(_) => Event::default().event("error").data("상태를 읽을 수 없습니다"),
            };
            Ok(event)
        }
    });

    Sse::new(stream).keep_alive(KeepAlive::default())
}

// --- metrics --------------------------------------------------------------

/// What a broadcast has cost so far, and what the machine is doing.
///
/// §9 of the brief and §22 of the original: the numbers a 24-hour test needs,
/// and the ones a price is eventually calculated from. Scoped to the caller —
/// the broadcasts are theirs, and the machine figures are the same ones `top`
/// would show anyone with a shell on the box.
#[derive(Serialize)]
pub struct Metrics {
    pub deployment: String,
    pub server: ServerMetrics,
    pub broadcasts: Vec<BroadcastMetrics>,
}

#[derive(Serialize)]
pub struct ServerMetrics {
    pub cpu_percent: f32,
    pub memory_total_bytes: u64,
    pub memory_available_bytes: u64,
    pub process_cpu_percent: f32,
    pub process_memory_bytes: u64,
    pub disk_available_bytes: u64,
    /// Bytes this account's broadcasts have pushed since each one started.
    /// The closest thing to "network out" that is actually attributable.
    pub egress_bytes: i64,
}

#[derive(Serialize)]
pub struct BroadcastMetrics {
    pub id: String,
    pub name: String,
    pub runtime_state: louver_cloud::RuntimeState,
    pub uptime_secs: i64,
    pub bytes_sent: i64,
    pub average_bitrate_bps: i64,
    pub restart_count: i64,
    pub last_error: Option<String>,
    pub ffmpeg_pid: Option<i64>,
    pub last_heartbeat: Option<String>,
}

pub async fn metrics(State(app): State<App>, Caller(uid): Caller) -> Out<Metrics> {
    let m = crate::blocking(move || {
        let broadcasts = app.db.broadcasts_for(&uid)?;
        let egress = broadcasts.iter().map(|b| b.bytes_sent).sum();

        // `sysinfo` needs two samples to have a CPU figure at all, and the
        // second must not be taken in the same instant as the first.
        let mut collector = louver_core::system::MetricsCollector::new();
        let _ = collector.sample(None);
        std::thread::sleep(std::time::Duration::from_millis(120));
        let s = collector.sample(None);

        Ok(Metrics {
            deployment: crate::health::deployment(),
            server: ServerMetrics {
                cpu_percent: s.system_cpu_percent,
                memory_total_bytes: s.total_memory_bytes,
                memory_available_bytes: s.available_memory_bytes,
                process_cpu_percent: s.app_cpu_percent,
                process_memory_bytes: s.app_memory_bytes,
                disk_available_bytes: louver_core::system::available_disk_bytes(&app.storage.scratch_dir()),
                egress_bytes: egress,
            },
            broadcasts: broadcasts
                .into_iter()
                .map(|b| BroadcastMetrics {
                    average_bitrate_bps: b.average_bitrate_bps(),
                    id: b.id,
                    name: b.name,
                    runtime_state: b.runtime_state,
                    uptime_secs: b.uptime_secs,
                    bytes_sent: b.bytes_sent,
                    restart_count: b.restart_count,
                    last_error: b.last_error,
                    ffmpeg_pid: b.ffmpeg_pid,
                    last_heartbeat: b.last_heartbeat,
                })
                .collect(),
        })
    })
    .await?;
    Ok(Json(m))
}
