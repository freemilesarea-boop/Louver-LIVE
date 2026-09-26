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
        let ingest = Ingest::new(db.clone(), Arc::clone(&storage), tools, encoder);

        Ok(Self { db, mgr, ingest, storage, keys, upload_tmp })
    }
}
