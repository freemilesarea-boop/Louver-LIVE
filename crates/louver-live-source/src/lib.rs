//! An isolated worker that sends a YouTube Live picture with the playlist's
//! sound. Beta.
//!
//! ## What this crate is, and what it is not
//!
//! It is a **separate process** with its own state file. It is not part of
//! `louver-server`, it does not open `cloud.db`, and it does not share an
//! FFmpeg child, a credential or a thread with the broadcasts running in
//! production today. The reason is measured rather than assumed: a live video
//! source cannot be stream-copied, so every one of these broadcasts is a real
//! x264 encode costing about a core, where an existing playlist broadcast costs
//! two to eight percent of one. Running them in the same process on the same
//! machine would take CPU away from paying customers who are on air.
//!
//! ## What it reuses
//!
//! Two things, verbatim, because having two copies of either would be the bug:
//!
//!  * [`louver_core`]'s `build_live_video_stream_args` — the composition that
//!    takes the picture from the live input and the sound from the playlist.
//!    [`args`] adds a 1080p cap to its output and changes nothing else.
//!  * [`louver_cloud::cctv::validate`] — the SSRF, private-range, credential
//!    and protocol checks. Every URL that reaches FFmpeg has been through it,
//!    including the one the resolver hands back.
//!
//! ## What it adds
//!
//!  * [`resolver`] — a YouTube watch address becomes a stream FFmpeg can open.
//!  * [`watchdog`] — noticing that the picture has frozen while the sound
//!    carries on, which measurement showed the existing pipeline cannot do.
//!  * [`limits`] — a ceiling on concurrent encodes, from the measured cost.
//!  * [`worker`] — one FFmpeg, owned by handle, with bounded restarts.
//!
//! ## Secrets
//!
//! Three things are treated as secret and have no field, parameter or log line
//! anywhere in this crate: the RTMP(S) destination (it contains the stream
//! key), the resolved manifest URL (YouTube signs it), and anything to do with
//! OAuth — this crate has no OAuth code at all and never touches a token.
//!
//! ## The security boundary
//!
//! Four checks, in this order, and a request has to pass all of the ones that
//! apply to it:
//!
//!  1. [`gate`] — did this request come through 247streams' proxy at all? One
//!     shared secret, constant-time. Not authentication, and never a substitute
//!     for it.
//!  2. [`origin`] — on the one route that reads a browser cookie, did the
//!     request come from the beta page? Exact match; missing and `null` are
//!     refused.
//!  3. [`auth`] / [`token`] — who is this? A session cookie once, for a
//!     five-minute bearer token, and that token on everything else.
//!  4. [`destinations`] and [`media`] — is this *their* destination, *their*
//!     file? Both are keyed by user id, and another user's name reads as not
//!     registered rather than as forbidden.

pub mod api;
pub mod args;
pub mod auth;
pub mod destinations;
pub mod error;
pub mod gate;
pub mod jobs;
pub mod limits;
pub mod media;
pub mod origin;
pub mod process;
pub mod resolver;
pub mod state;
pub mod token;
pub mod watchdog;
pub mod worker;

pub use api::Api;
pub use auth::{Identity, IdentitySource, ProductionMe};
pub use destinations::Destinations;
pub use error::{ErrorKind, LiveSourceError, Result};
pub use gate::Gate;
pub use jobs::{JobView, NewJob, Registry, Settings};
pub use limits::Limits;
pub use media::{MediaRoot, StoredMedia};
pub use origin::AllowedOrigins;
pub use resolver::{classify, LiveSourceResolver, ResolvedSource, SourceKind, YtDlpResolver};
pub use state::{Desired, Phase, StateStore, WorkerState};
pub use token::Signer;
pub use watchdog::{FrameWatchdog, Verdict};
pub use worker::{LiveWorker, Outcome, WorkerConfig};
