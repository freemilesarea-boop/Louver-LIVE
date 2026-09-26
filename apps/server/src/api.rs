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
    pub media_id: String,
    pub destination_id: String,
    #[serde(default = "yes")]
    pub loop_forever: bool,
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

pub async fn create_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Json(b): Json<NewBroadcast>,
) -> Out<Broadcast> {
    Ok(Json(
        crate::blocking(move || {
            app.db.create_broadcast(&uid, b.name.trim(), &b.media_id, &b.destination_id, b.loop_forever)
        })
        .await?,
    ))
}

pub async fn get_broadcast(
    State(app): State<App>,
    Caller(uid): Caller,
    Path(id): Path<String>,
) -> Out<Broadcast> {
    Ok(Json(crate::blocking(move || app.db.broadcast_owned(&uid, &id)).await?))
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
