//! What every request handler is given.

use louver_cloud::credentials::CredentialStore;
use louver_cloud::ingest::Ingest;
use louver_cloud::manager::{BroadcastManager, FfmpegLaunchers};
use louver_cloud::storage::{LocalStorage, Storage};
use louver_cloud::{CloudDb, Result};
use louver_core::security::SecretStore;
use louver_core::streaming::ffmpeg::FfmpegTools;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone)]
pub struct App {
    pub db: CloudDb,
    pub mgr: BroadcastManager,
    pub ingest: Ingest,
    pub storage: Arc<dyn Storage>,
    pub keys: Arc<dyn SecretStore>,
    pub upload_tmp: PathBuf,
    /// Kept so `/health` can ask FFmpeg whether it runs, rather than checking
    /// that a file exists where one was once found.
    pub tools: FfmpegTools,
    /// The YouTube provider, when this server has Google credentials. `None`
    /// is a supported deployment, not a broken one: manual RTMPS is the path
    /// that is on air, and it needs nothing from Google.
    pub youtube: Option<louver_cloud::youtube::Youtube>,
    /// The payment provider, when this server has PayApp credentials. `None` is
    /// again a supported deployment: everything but paying works, and the
    /// checkout route is the only thing that has to say so.
    pub payapp: Option<louver_cloud::billing::Payapp>,
    /// CPU and memory, sampled on demand for the admin console.
    ///
    /// One long-lived sampler rather than a fresh one per request: `sysinfo`
    /// reports CPU as the change between two readings, so a sampler created for
    /// a single request always answers zero. Behind a mutex because it is
    /// stateful, and the admin page is the only caller.
    pub machine: std::sync::Arc<Machine>,
}

/// What this container is using, without spawning anything.
#[derive(Debug)]
pub struct Machine(std::sync::Mutex<sysinfo::System>);

impl Default for Machine {
    fn default() -> Self {
        Self(std::sync::Mutex::new(sysinfo::System::new()))
    }
}

impl Machine {
    /// `(cpu percent of the whole machine, used MB, total MB)`.
    pub fn sample(&self) -> (f32, u64, u64) {
        let mut sys = match self.0.lock() {
            Ok(s) => s,
            Err(e) => e.into_inner(),
        };
        sys.refresh_cpu();
        sys.refresh_memory();
        let cpu = sys.global_cpu_info().cpu_usage();
        (cpu, sys.used_memory() / 1_048_576, sys.total_memory() / 1_048_576)
    }
}

impl App {
    /// Build everything from the environment, and fail loudly on what is missing.
    pub fn boot() -> Result<Self> {
        let data =
            PathBuf::from(std::env::var("LOUVER_DATA_DIR").unwrap_or_else(|_| "/var/lib/louver".into()));
        std::fs::create_dir_all(&data)?;

        let db = CloudDb::open(&data.join("cloud.db"))?;

        // No key, no boot. Inventing one would quietly turn every saved stream
        // key into unreadable bytes.
        let master = louver_cloud::credentials::master_key_from_env()?;
        let keys: Arc<dyn SecretStore> = Arc::new(CredentialStore::new(db.raw(), master));

        let storage: Arc<dyn Storage> = Arc::new(LocalStorage::new(data.join("media")));
        let upload_tmp = data.join("uploads");
        std::fs::create_dir_all(&upload_tmp)?;
        sweep_abandoned_uploads(&upload_tmp);

        let tools =
            FfmpegTools::discover(std::env::var("LOUVER_FFMPEG_DIR").ok().map(PathBuf::from).as_deref())
                .map_err(|e| louver_cloud::CloudError::Invalid(format!("FFmpeg를 찾지 못했습니다: {e}")))?;
        let encoder = std::env::var("LOUVER_ENCODER").unwrap_or_else(|_| "libx264".into());

        let mgr = BroadcastManager::new(
            db.clone(),
            Arc::clone(&storage),
            data.join("work"),
            tools.clone(),
            encoder.clone(),
            Arc::clone(&keys),
            Arc::new(FfmpegLaunchers { program: tools.ffmpeg.clone() }),
        );
        // §2: the client id and secret come from the environment. Without them
        // the routes answer "not configured" and everything else is unaffected.
        let youtube = build_youtube(&db, &keys);
        let mgr = match &youtube {
            Some(yt) => mgr.with_youtube(yt.clone()),
            None => mgr,
        };

        let ingest = Ingest::new(db.clone(), Arc::clone(&storage), tools.clone(), encoder);

        // Same rule as YouTube: absent credentials make one feature unavailable,
        // not the server unbootable. A production box that has not been given
        // PayApp keys yet still streams.
        let payapp = match louver_cloud::billing::Config::from_env() {
            Ok(config) => Some(louver_cloud::billing::Payapp::new(
                db.clone(),
                Arc::new(louver_cloud::billing::UreqForm),
                config,
            )),
            Err(e) => {
                println!("[louver] 결제: 설정되지 않았습니다 ({e})");
                None
            }
        };

        Ok(Self {
            db,
            mgr,
            ingest,
            storage,
            keys,
            upload_tmp,
            tools,
            youtube,
            payapp,
            machine: std::sync::Arc::new(Machine::default()),
        })
    }
}

/// Throw away `.part` files from uploads that never finished.
///
/// The handler deletes its own temp file on both the success and the failure
/// path, so these only exist when the process died mid-upload — a deploy, a
/// crash, an OOM. Each one is as large as the video that was being sent, and
/// nothing would ever look at them again. Boot is the one moment when no upload
/// is in flight and deleting them is unambiguously safe.
fn sweep_abandoned_uploads(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut freed: u64 = 0;
    let mut count = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("part") {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if std::fs::remove_file(&path).is_ok() {
            freed += size;
            count += 1;
        }
    }
    if count > 0 {
        println!("[louver] 중단된 업로드 {count}개를 정리했습니다 ({}MB)", freed / 1_048_576);
    }
}

/// The provider, if the environment configured one.
///
/// `UreqClient` is both the API transport and the token endpoint, which is why
/// one value is behind both traits. `YOUTUBE_TOKEN_ENDPOINT` exists so an
/// end-to-end test can point the flow at a fake Google; in production it is
/// unset and Google's own endpoint is used.
fn build_youtube(db: &CloudDb, keys: &Arc<dyn SecretStore>) -> Option<louver_cloud::youtube::Youtube> {
    let config = match louver_cloud::youtube::Config::from_env() {
        Ok(c) => c,
        Err(_) => return None,
    };
    let transport = Arc::new(louver_core::youtube::http::UreqClient {
        token_endpoint: std::env::var("YOUTUBE_TOKEN_ENDPOINT").ok().filter(|s| !s.trim().is_empty()),
    });
    Some(louver_cloud::youtube::Youtube::new(
        db.clone(),
        Arc::clone(keys),
        Arc::clone(&transport) as Arc<dyn louver_core::youtube::HttpClient>,
        transport as Arc<dyn louver_core::youtube::oauth::TokenEndpoint>,
        config,
    ))
}
