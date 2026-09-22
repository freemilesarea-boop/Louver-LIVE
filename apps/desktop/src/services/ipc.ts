/**
 * The single place the UI talks to Rust.
 *
 * Every call goes through `call()`, which normalizes rejections into a
 * {@link LouverError} so the UI can always show a Korean message with the
 * technical detail tucked behind a disclosure (§35).
 *
 * When `window.__TAURI_INTERNALS__` is absent — a browser `npm run dev`, or the
 * test suite — calls are routed to a registered mock backend instead. This is
 * what lets the UI e2e suite (§58) drive the real components without a
 * packaged binary.
 */
import type {
  AddResult, DashboardMetrics, LouverError,
  Media, Playlist, PlaylistView, PreflightReport, RuntimeStatus, ScheduleView,
  SettingsView, StreamDiagnostics, StreamEvent, SchedulerStatusView,
  BroadcastMetadata, BroadcastPreset, ChatMessage, ChatSettings, ChatStatus,
  LiveBroadcast, YoutubeStatus, MetadataOutcome, MetadataApplyState, ApplyPlan, QuotaReport,
} from '@/types'

export type MockBackend = (cmd: string, args: Record<string, unknown>) => unknown | Promise<unknown>

let mockBackend: MockBackend | null = null

/** Install an in-process backend. Used by the dev server and by tests. */
export function setMockBackend(b: MockBackend | null) {
  mockBackend = b
}

export function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
}

function toLouverError(e: unknown): LouverError {
  if (e && typeof e === 'object' && 'code_str' in e && 'message' in e) {
    return e as LouverError
  }
  return {
    code_str: 'LL-UNKNOWN',
    message: typeof e === 'string' ? e : '알 수 없는 오류가 발생했습니다.',
    detail: e instanceof Error ? e.message : undefined,
  }
}

export async function call<T>(cmd: string, args: Record<string, unknown> = {}): Promise<T> {
  try {
    if (isTauri()) {
      const { invoke } = await import('@tauri-apps/api/core')
      return (await invoke(cmd, args)) as T
    }
    if (mockBackend) return (await mockBackend(cmd, args)) as T
    throw new Error(`no backend available for "${cmd}"`)
  } catch (e) {
    throw toLouverError(e)
  }
}

/** Subscribe to a Rust-emitted event. Returns an unsubscribe function. */
export async function listen<T>(event: string, cb: (payload: T) => void): Promise<() => void> {
  if (isTauri()) {
    const { listen: tauriListen } = await import('@tauri-apps/api/event')
    const un = await tauriListen<T>(event, (e) => cb(e.payload))
    return un
  }
  const handler = (e: Event) => cb((e as CustomEvent<T>).detail)
  window.addEventListener(event, handler)
  return () => window.removeEventListener(event, handler)
}

/** Emit an event locally; the mock backend uses this to push status updates. */
export function emitLocal<T>(event: string, payload: T) {
  window.dispatchEvent(new CustomEvent(event, { detail: payload }))
}

// --- typed command surface --------------------------------------------------

export const api = {
  // media
  /**
   * Add and prepare in one call, so no second button exists (§1, §5).
   *
   * There is no `importMedia` beside this one. `import_media` and
   * `estimate_optimization` are still Rust commands — `add_media` calls the
   * first and `optimize_media` calls the second for its disk guard — but
   * nothing in the UI reaches either directly any more, and a binding no
   * screen uses is a binding no test covers.
   */
  addMedia: (paths: string[]) => call<AddResult>('add_media', { paths }),
  listMedia: () => call<Media[]>('list_media'),
  deleteMedia: (id: number) => call<void>('delete_media', { id }),
  compatibilityReasons: (id: number) => call<string[]>('compatibility_reasons', { id }),
  optimizeMedia: (mediaIds: number[]) => call<number>('optimize_media', { mediaIds }),
  cancelOptimization: () => call<void>('cancel_optimization'),

  // playlists
  listPlaylists: () => call<Playlist[]>('list_playlists'),
  getPlaylist: (id: number) => call<PlaylistView | null>('get_playlist', { id }),
  createPlaylist: (name: string) => call<number>('create_playlist', { name }),
  updatePlaylist: (id: number, name: string, playbackMode: string, outputProfile: string) =>
    call<void>('update_playlist', { id, name, playbackMode, outputProfile }),
  deletePlaylist: (id: number) => call<void>('delete_playlist', { id }),
  addToPlaylist: (playlistId: number, mediaIds: number[]) =>
    call<void>('add_to_playlist', { playlistId, mediaIds }),
  reorderPlaylist: (playlistId: number, itemIds: number[]) =>
    call<void>('reorder_playlist', { playlistId, itemIds }),
  setItemEnabled: (itemId: number, enabled: boolean) =>
    call<void>('set_item_enabled', { itemId, enabled }),
  removePlaylistItem: (itemId: number) => call<void>('remove_playlist_item', { itemId }),

  // schedules
  listSchedules: () => call<ScheduleView[]>('list_schedules'),
  createSchedule: (playlistId: number, daysOfWeek: number, startTime: string, endTime: string) =>
    call<number>('create_schedule', { playlistId, daysOfWeek, startTime, endTime }),
  updateSchedule: (
    id: number, playlistId: number, daysOfWeek: number,
    startTime: string, endTime: string, enabled: boolean,
  ) => call<void>('update_schedule', { id, playlistId, daysOfWeek, startTime, endTime, enabled }),
  deleteSchedule: (id: number) => call<void>('delete_schedule', { id }),
  schedulerStatus: () => call<SchedulerStatusView>('scheduler_status'),
  schedulerArm: () => call<SchedulerStatusView>('scheduler_arm'),
  schedulerDisarm: () => call<SchedulerStatusView>('scheduler_disarm'),
  schedulerSetRestore: (enabled: boolean) => call<void>('scheduler_set_restore', { enabled }),

  // streaming
  getStatus: () => call<RuntimeStatus>('get_status'),
  runPreflight: (playlistId: number, dryRun: boolean) =>
    call<PreflightReport>('run_preflight', { playlistId, dryRun }),
  startBroadcast: (playlistId: number, skipYoutube = false) =>
    call<RuntimeStatus>('start_broadcast', { playlistId, skipYoutube }),
  stopBroadcast: () => call<RuntimeStatus>('stop_broadcast'),
  startDryRun: (playlistId: number) => call<RuntimeStatus>('start_dry_run', { playlistId }),
  streamModeLabel: () => call<string>('stream_mode_label'),
  streamDiagnostics: () => call<StreamDiagnostics>('stream_diagnostics'),
  simulateCrash: () => call<void>('simulate_ffmpeg_crash'),

  // settings
  getSettings: () => call<SettingsView>('get_settings'),
  setSetting: (key: string, value: string) => call<void>('set_setting', { key, value }),
  setStreamKey: (key: string) => call<void>('set_stream_key', { key }),
  revealStreamKey: () => call<string>('reveal_stream_key'),
  clearStreamKey: () => call<void>('clear_stream_key'),

  // system
  getMetrics: () => call<DashboardMetrics>('get_metrics'),
  recentEvents: (limit?: number) => call<StreamEvent[]>('recent_events', { limit }),
  readLog: (target: string, lines?: number) => call<string[]>('read_log', { target, lines }),
  logsDirectory: () => call<string>('logs_directory'),
  clearCache: () => call<string>('clear_cache'),
  cacheInUseCount: () => call<number>('cache_in_use_count'),
  takeStartupNotice: () => call<string | null>('take_startup_notice'),
  uptimeWarnings: () => call<string[]>('uptime_warnings'),

  // --- youtube (V2) ---
  youtubeStatus: () => call<YoutubeStatus>('youtube_status'),
  youtubeSetCredentials: (clientId: string, clientSecret: string) =>
    call<YoutubeStatus>('youtube_set_credentials', { clientId, clientSecret }),
  youtubeBeginConnect: () => call<string>('youtube_begin_connect'),
  youtubeSwitchAccount: () => call<string>('youtube_switch_account'),
  youtubeDisconnect: () => call<YoutubeStatus>('youtube_disconnect'),
  youtubeGetMetadata: () => call<BroadcastMetadata>('youtube_get_metadata'),
  youtubeSaveMetadata: (metadata: BroadcastMetadata) =>
    call<BroadcastMetadata>('youtube_save_metadata', { metadata }),
  youtubeApplyMetadata: () => call<MetadataOutcome>('youtube_apply_metadata'),
  youtubeApplyState: () => call<MetadataApplyState>('youtube_apply_state'),
  youtubeQuota: () => call<QuotaReport>('youtube_quota'),
  youtubeScheduleHolds: () => call<boolean>('youtube_schedule_holds'),
  youtubeSetScheduleHolds: (holds: boolean) => call<void>('youtube_set_schedule_holds', { holds }),
  youtubeApplyPlan: () => call<ApplyPlan>('youtube_apply_plan'),
  youtubeSetApplyOnStart: (enabled: boolean) =>
    call<void>('youtube_set_apply_on_start', { enabled }),
  youtubeCurrentBroadcast: () => call<LiveBroadcast>('youtube_current_broadcast'),
  youtubeListPresets: () => call<BroadcastPreset[]>('youtube_list_presets'),
  youtubeSavePreset: (name: string, metadata: BroadcastMetadata) =>
    call<BroadcastPreset[]>('youtube_save_preset', { name, metadata }),
  youtubeDeletePreset: (id: number) => call<BroadcastPreset[]>('youtube_delete_preset', { id }),
  chatListMessages: () => call<ChatMessage[]>('chat_list_messages'),
  chatAddMessage: (text: string) => call<ChatMessage[]>('chat_add_message', { text }),
  chatUpdateMessage: (id: number, text: string, enabled: boolean) =>
    call<ChatMessage[]>('chat_update_message', { id, text, enabled }),
  chatDeleteMessage: (id: number) => call<ChatMessage[]>('chat_delete_message', { id }),
  chatReorderMessages: (ids: number[]) => call<ChatMessage[]>('chat_reorder_messages', { ids }),
  chatGetSettings: () => call<ChatSettings>('chat_get_settings'),
  chatSaveSettings: (settings: ChatSettings) => call<ChatSettings>('chat_save_settings', { settings }),
  chatStatus: () => call<ChatStatus>('chat_status'),
  chatMinIntervalSecs: () => call<number>('chat_min_interval_secs'),
}

/** Open the native file picker, or fall back to a prompt outside Tauri. */
export async function pickVideoFiles(): Promise<string[]> {
  if (isTauri()) {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const r = await open({
      multiple: true,
      filters: [{ name: '영상 파일', extensions: ['mp4', 'mov', 'mkv', 'm4v', 'webm', 'avi', 'ts'] }],
    })
    if (!r) return []
    return Array.isArray(r) ? r : [r]
  }
  return call<string[]>('__pick_files')
}

/**
 * Open a URL in the user's browser.
 *
 * Not `revealItemInDir`, which shows a *file* in a folder and fails on a URL.
 * The capability file permits exactly one host — Google's consent screen — so
 * this cannot be turned into a way of opening arbitrary links.
 */
export async function openUrl(url: string): Promise<void> {
  if (isTauri()) {
    const { openUrl: open } = await import('@tauri-apps/plugin-opener')
    await open(url)
    return
  }
  await call<void>('__open_url', { url })
}

export async function revealPath(path: string): Promise<void> {
  if (isTauri()) {
    const { revealItemInDir } = await import('@tauri-apps/plugin-opener')
    await revealItemInDir(path)
    return
  }
  await call<void>('__reveal', { path })
}
