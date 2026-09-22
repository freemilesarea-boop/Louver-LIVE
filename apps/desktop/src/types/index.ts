/** Mirrors the serde representations in `louver-core`. */

export type StreamState =
  | 'IDLE' | 'PREPARING' | 'CONNECTING' | 'LIVE'
  | 'RECONNECTING' | 'STOPPING' | 'STOPPED' | 'ERROR'

export type MediaStatus =
  | 'imported' | 'compatible' | 'optimization_required'
  | 'normalized' | 'missing' | 'failed'

export type PlaybackMode = 'sequential' | 'shuffle_once'
export type StreamMode = 'stream_copy' | 'compatibility_encode'
export type StartReason = 'manual' | 'scheduled' | 'recovered'
export type CheckOutcome = 'pass' | 'warn' | 'fail'
export type EventLevel = 'info' | 'warn' | 'error'

/** The structured error every command rejects with (§35). */
export interface LouverError {
  code_str: string
  message: string
  detail?: string | null
}

export interface Media {
  id: number
  source_path: string
  display_name: string
  status: MediaStatus
  media_hash: string
  normalized_path?: string | null
  normalized_profile?: string | null
  duration_secs: number
  normalized_duration_secs?: number | null
  width: number
  height: number
  fps: number
  video_codec: string
  audio_codec?: string | null
  pixel_format?: string | null
  is_hdr: boolean
  file_size: number
  added_at: string
  last_error?: string | null
}

export interface Playlist {
  id: number
  name: string
  playback_mode: PlaybackMode
  output_profile: string
  created_at: string
  updated_at: string
}

export interface PlaylistItemView {
  id: number
  playlist_id: number
  media_id: number
  position: number
  enabled: boolean
  media: Media
}

export interface PlaylistView {
  playlist: Playlist
  items: PlaylistItemView[]
  total_duration_secs: number
  total_duration_label: string
  unready_count: number
}

export interface ScheduleView {
  id: number
  playlist_id: number
  days_of_week: number
  start_time: string
  end_time: string
  enabled: boolean
  playlist_name?: string | null
  days_label: string
  crosses_midnight: boolean
  next_start?: string | null
  next_end?: string | null
  window_duration_label: string
  /** The window is open right now — which `next_start` alone cannot say. */
  active_now: boolean
  active_until?: string | null
  playlist_missing: boolean
  playlist_ready_count: number
}

export interface StreamProgress {
  frames: number
  fps: number
  bitrate_kbps: number
  total_bytes: number
  out_time_ms: number
  speed: number
}

export interface SupervisorStatus {
  state: StreamState
  mode: StreamMode
  pid?: number | null
  restart_count: number
  reconnect_count: number
  seconds_since_data?: number | null
  progress: StreamProgress
  last_error?: LouverError | null
  next_retry_in_secs?: number | null
}

export interface RuntimeStatus {
  supervisor: SupervisorStatus
  playlist_id?: number | null
  playlist_name?: string | null
  current_item?: string | null
  next_item?: string | null
  current_index?: number | null
  item_count: number
  elapsed_secs: number
  remaining_secs?: number | null
  scheduled_end?: string | null
  start_reason?: StartReason | null
  dry_run: boolean
  next_scheduled_start?: string | null
  cycle_duration_secs: number
  /** Why the last start attempt failed. Cleared by the next successful start. */
  last_start_error?: LouverError | null
  /**
   * The scheduled window that is open right now, whether or not it is
   * broadcasting yet — so the dashboard can never say "예약된 방송이 없습니다"
   * while the schedule page says the window is open.
   */
  active_occurrence?: OccurrenceStatus | null
  /** What this computer is doing about scheduled broadcasts. */
  scheduler_state: SchedulerState
}

/**
 * Saving a schedule and running a scheduler are different things. One is a
 * rule in a database; the other is this computer watching a clock.
 */
export type SchedulerState =
  | 'STOPPED' | 'ARMING' | 'WAITING' | 'STARTING' | 'LIVE' | 'STOPPING' | 'ERROR'

export interface SchedulerStatusView {
  state: SchedulerState
  armed: boolean
  enabled_count: number
  next_start?: string | null
  next_end?: string | null
  next_playlist?: string | null
  seconds_until_start?: number | null
  active_start?: string | null
  active_end?: string | null
  last_error?: LouverError | null
  restore_on_launch: boolean
}

export interface OccurrenceStatus {
  start: string
  end: string
  playlist_id: number
  phase: 'preparing' | 'live' | 'stopping'
  retry_in_secs?: number | null
  attempts: number
}

export interface StreamDiagnostics {
  state: StreamState
  configured_mode: StreamMode
  argv_is_stream_copy: boolean
  mismatch?: string | null
  video_encoder_args: string[]
  masked_command: string[]
  ffmpeg_pid?: number | null
  ffmpeg_cpu_percent: number
  verdict: string
  ffmpeg_memory_bytes: number
  reconnect_count: number
  seconds_since_progress?: number | null
  publishing: boolean
  bytes_sent: number
  local_test_sink: LocalTestSink
}

/** Where a local test is publishing. */
export type LocalTestSink =
  | { kind: 'None' }
  | { kind: 'Rtmp'; target: string }
  | { kind: 'File'; target: string }

export interface CheckResult {
  id: string
  label: string
  outcome: CheckOutcome
  detail: string
  code?: string | null
}

export interface PreflightReport {
  checks: CheckResult[]
  can_broadcast: boolean
}

export interface DiskEstimate {
  files_to_process: number
  total_duration_secs: number
  estimated_bytes: number
  available_bytes: number
  has_enough_space: boolean
  safety_margin_bytes: number
}

export interface NormalizeProgress {
  media_id: number
  file_name: string
  percent: number
  files_done: number
  files_total: number
  remaining_files: number
  /** What is being done to this file, already in Korean. */
  mode_label: string
  /** Seconds of video produced per second of wall clock. */
  speed_x: number
  /** Seconds left for the whole batch; negative until it can be estimated. */
  eta_secs: number
  /** "NVIDIA GPU", "Intel Quick Sync", "CPU", "변환 없음". */
  engine_label: string
  estimated_cache_bytes: number
}

export interface ProfileOption {
  id: string
  label: string
  video_kbps: number
}

export interface SettingsView {
  rtmps_url: string
  output_profile: string
  stream_mode: StreamMode
  launch_at_startup: boolean
  start_minimized: boolean
  minimize_to_tray: boolean
  auto_reconnect: boolean
  developer_mode: boolean
  first_run_complete: boolean
  active_playlist?: number | null
  cache_location: string
  cache_size_bytes: number
  cache_size_label: string
  stream_key_masked: string
  stream_key_hint?: string | null
  has_stream_key: boolean
  secret_backend: string
  secret_backend_is_secure: boolean
  ffmpeg_path?: string | null
  ffmpeg_version?: string | null
  hardware_encoder: string
  logs_dir: string
  app_version: string
  profiles: ProfileOption[]
}

export interface DashboardMetrics {
  app_cpu_percent: number
  app_memory_bytes: number
  system_cpu_percent: number
  total_memory_bytes: number
  available_memory_bytes: number
  ffmpeg_cpu_percent: number
  ffmpeg_memory_bytes: number
  app_memory_label: string
  cache_bytes: number
  cache_label: string
  free_disk_bytes: number
  free_disk_label: string
  sleep_prevented: boolean
  ffmpeg_ready: boolean
  ffprobe_ready: boolean
  stream_mode_label: string
}

export interface StreamEvent {
  id: number
  session_id?: number | null
  at: string
  level: EventLevel
  code?: string | null
  message: string
}

export interface ImportResult {
  imported: Media[]
  failed: { path: string; code: string; message: string }[]
}

/** What `add_media` did: files added, and what it took to make them usable. */
export interface AddResult extends ImportResult {
  /** Needed nothing done to them at all. */
  ready_at_once: number
  /** Were prepared, whether by a container rewrite or an encode. */
  prepared: number
}

// --- YouTube (V2) ---------------------------------------------------------

export type Privacy = 'public' | 'unlisted' | 'private'

export interface BroadcastMetadata {
  title: string
  description: string
  tags: string[]
  category_id: string
  privacy: Privacy
}

export interface BroadcastPreset extends BroadcastMetadata {
  id: number
  name: string
}

export interface YoutubeStatus {
  connected: boolean
  channel_id?: string | null
  channel_title?: string | null
  has_credentials: boolean
  apply_on_start: boolean
  using_custom_client: boolean
  /** Whether this build carries a client secret. Never the value itself. */
  has_client_secret: boolean
  last_auth_diagnostic?: string | null
  client_id_hint?: string | null
  secret_backend: string
  secret_backend_is_secure: boolean
  connecting_error?: string | null
}

export interface LiveBroadcast {
  id: string
  title: string
  privacy: Privacy
  active_live_chat_id?: string | null
  life_cycle_status: string
}

/** One field of the metadata, as Google reports it back after the update. */
export interface FieldCheck {
  applied: boolean
  actual: string
}

export interface MetadataVerification {
  title: FieldCheck
  description: FieldCheck
  tags: FieldCheck
  category: FieldCheck
  privacy: FieldCheck
}

export interface MetadataOutcome {
  broadcast: LiveBroadcast
  verification: MetadataVerification
}

export type ApplyStage =
  | 'off' | 'not_connected' | 'applying' | 'skipped' | 'quota_exhausted'
  | 'applied' | 'mismatch' | 'failed'

/**
 * The day's free-quota spending. There is no paid tier behind it: the project
 * carries no billing account, so running out pauses the optional features
 * until the quota resets, and never produces a charge.
 */
export interface QuotaReport {
  used_percent: number
  spent: number
  cap: number
  exhausted: boolean
  day: string
  /** Methods with their own daily allowance, counted in calls rather than units. */
  buckets: BucketReport[]
}

export interface BucketReport {
  key: string
  calls: number
  daily_calls: number
  exhausted: boolean
}

/**
 * The YouTube half of a broadcast, tracked separately from the stream's own
 * state: a connected RTMPS stream says nothing about whether the title changed.
 */
export interface MetadataApplyState {
  stage: ApplyStage
  broadcast_id?: string | null
  requested?: BroadcastMetadata | null
  verification?: MetadataVerification | null
  error?: LouverError | null
  /**
   * Which preparation step failed, in the user's words — "예약 방송 생성 실패"
   * rather than the one sentence every YouTube error used to share.
   */
  failed_stage?: string | null
  /** What to try, shown under the stage. */
  failed_remedy?: string | null
  /** 'manual' or 'scheduled': who started this attempt. */
  origin?: string | null
  /** Every step of the attempt, in order. */
  steps?: StepRecord[]
}

export type ProvisionStep =
  | 'token_refresh' | 'broadcast_list' | 'broadcast_insert' | 'stream_list'
  | 'broadcast_bind' | 'metadata_apply' | 'stream_active' | 'broadcast_transition'

export type StepOutcome = 'started' | 'ok' | 'failed' | 'skipped'

/** One named call in getting a broadcast ready, and how it went. */
export interface StepRecord {
  step: ProvisionStep
  outcome: StepOutcome
  detail?: string | null
  error_code?: string | null
}

/** Whether the next Start can do the YouTube work the user asked for. */
export interface ApplyPlan {
  wanted: boolean
  connected: boolean
  chat_enabled: boolean
}

export interface ChatMessage {
  id: number
  position: number
  text: string
  enabled: boolean
}

export type ChatOrder = 'sequential' | 'random'

export interface ChatSettings {
  enabled: boolean
  order: ChatOrder
  interval_secs: number
  send_on_start: boolean
  send_on_end: boolean
  avoid_repeats: boolean
}

export type ChatState =
  | 'IDLE' | 'WAITING_FOR_LIVE_CHAT' | 'CONNECTED' | 'SENDING' | 'PAUSED' | 'ERROR'

export interface ChatStatus {
  state: ChatState
  state_label: string
  live_chat_id_known: boolean
  messages_sent: number
  seconds_until_next?: number | null
  last_error?: string | null
  last_error_code?: string | null
  broadcast_id?: string | null
  broadcast_title?: string | null
  broadcast_privacy?: Privacy | null
}
