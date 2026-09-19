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
  DashboardMetrics, DiskEstimate, ImportResult, LicenseState, LouverError,
  Media, Playlist, PlaylistView, PreflightReport, RuntimeStatus, ScheduleView,
  SettingsView, StreamEvent,
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
  importMedia: (paths: string[]) => call<ImportResult>('import_media', { paths }),
  listMedia: () => call<Media[]>('list_media'),
  deleteMedia: (id: number) => call<void>('delete_media', { id }),
  compatibilityReasons: (id: number) => call<string[]>('compatibility_reasons', { id }),
  estimateOptimization: (mediaIds: number[]) =>
    call<DiskEstimate>('estimate_optimization', { mediaIds }),
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

  // streaming
  getStatus: () => call<RuntimeStatus>('get_status'),
  runPreflight: (playlistId: number, dryRun: boolean) =>
    call<PreflightReport>('run_preflight', { playlistId, dryRun }),
  startBroadcast: (playlistId: number) => call<RuntimeStatus>('start_broadcast', { playlistId }),
  stopBroadcast: () => call<RuntimeStatus>('stop_broadcast'),
  startDryRun: (playlistId: number) => call<RuntimeStatus>('start_dry_run', { playlistId }),
  streamModeLabel: () => call<string>('stream_mode_label'),
  simulateCrash: () => call<void>('simulate_ffmpeg_crash'),

  // settings
  getSettings: () => call<SettingsView>('get_settings'),
  setSetting: (key: string, value: string) => call<void>('set_setting', { key, value }),
  setStreamKey: (key: string) => call<void>('set_stream_key', { key }),
  revealStreamKey: () => call<string>('reveal_stream_key'),
  clearStreamKey: () => call<void>('clear_stream_key'),
  getLicense: () => call<LicenseState>('get_license'),
  installLicense: (path: string) => call<LicenseState>('install_license', { path }),

  // system
  getMetrics: () => call<DashboardMetrics>('get_metrics'),
  recentEvents: (limit?: number) => call<StreamEvent[]>('recent_events', { limit }),
  readLog: (target: string, lines?: number) => call<string[]>('read_log', { target, lines }),
  logsDirectory: () => call<string>('logs_directory'),
  clearCache: () => call<string>('clear_cache'),
  cacheInUseCount: () => call<number>('cache_in_use_count'),
  takeStartupNotice: () => call<string | null>('take_startup_notice'),
  uptimeWarnings: () => call<string[]>('uptime_warnings'),
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

export async function pickLicenseFile(): Promise<string | null> {
  if (isTauri()) {
    const { open } = await import('@tauri-apps/plugin-dialog')
    const r = await open({ multiple: false, filters: [{ name: '라이선스', extensions: ['json'] }] })
    return typeof r === 'string' ? r : null
  }
  return call<string | null>('__pick_license')
}

export async function revealPath(path: string): Promise<void> {
  if (isTauri()) {
    const { revealItemInDir } = await import('@tauri-apps/plugin-opener')
    await revealItemInDir(path)
    return
  }
  await call<void>('__reveal', { path })
}
