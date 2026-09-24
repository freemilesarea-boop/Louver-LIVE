/** Global UI state. Data lives in Rust; this holds what the UI is showing. */
import { create } from 'zustand'
import { api, listen } from '@/services/ipc'
import type {
  DashboardMetrics, LouverError, Media, NormalizeProgress, Playlist,
  PlaylistView, RuntimeStatus, ScheduleView, SettingsView,
} from '@/types'

export type Page = 'dashboard' | 'playlist' | 'schedule' | 'broadcast' | 'logs' | 'settings'

export interface Toast {
  id: number
  kind: 'info' | 'error' | 'success'
  message: string
  detail?: string | undefined
  code?: string | undefined
}

interface AppState {
  page: Page
  setPage: (p: Page) => void

  status: RuntimeStatus | null
  metrics: DashboardMetrics | null
  settings: SettingsView | null
  playlists: Playlist[]
  activePlaylistId: number | null
  activePlaylist: PlaylistView | null
  media: Media[]
  schedules: ScheduleView[]
  normalizing: NormalizeProgress | null
  toasts: Toast[]
  startupNotice: string | null
  booted: boolean

  setActivePlaylist: (id: number | null) => Promise<void>
  refreshAll: () => Promise<void>
  refreshStatus: () => Promise<void>
  refreshMetrics: () => Promise<void>
  refreshPlaylists: () => Promise<void>
  refreshActivePlaylist: () => Promise<void>
  refreshMedia: () => Promise<void>
  refreshSchedules: () => Promise<void>
  refreshSettings: () => Promise<void>

  toast: (t: Omit<Toast, 'id'>) => void
  reportError: (e: unknown) => void
  dismissToast: (id: number) => void
  dismissNotice: () => void
  subscribe: () => Promise<() => void>
}

function asLouverError(e: unknown): LouverError {
  const err = e as Partial<LouverError>
  return {
    code_str: err?.code_str ?? 'LL-UNKNOWN',
    message: err?.message ?? '알 수 없는 오류가 발생했습니다.',
    detail: err?.detail ?? undefined,
  }
}

let toastSeq = 1

export const useAppStore = create<AppState>((set, get) => ({
  page: 'dashboard',
  setPage: (page) => set({ page }),

  status: null,
  metrics: null,
  settings: null,
  playlists: [],
  activePlaylistId: null,
  activePlaylist: null,
  media: [],
  schedules: [],
  normalizing: null,
  toasts: [],
  startupNotice: null,
  booted: false,

  setActivePlaylist: async (id) => {
    set({ activePlaylistId: id })
    if (id != null) {
      await api.setSetting('active_playlist', String(id)).catch(() => {})
      await get().refreshActivePlaylist()
    } else {
      set({ activePlaylist: null })
    }
  },

  refreshAll: async () => {
    await Promise.all([
      get().refreshStatus(),
      get().refreshPlaylists(),
      get().refreshMedia(),
      get().refreshSchedules(),
      get().refreshSettings(),
      get().refreshMetrics(),
    ])
    // Restore the playlist the user last worked with (§58: settings survive
    // a restart).
    const { settings, playlists, activePlaylistId } = get()
    if (activePlaylistId == null) {
      const saved = settings?.active_playlist ?? null
      const stillExists = saved != null && playlists.some((p) => p.id === saved)
      const id = stillExists ? saved : playlists[0]?.id ?? null
      if (id != null) await get().setActivePlaylist(id)
    }
    const notice = await api.takeStartupNotice().catch(() => null)
    set({ startupNotice: notice ?? null, booted: true })
  },

  refreshStatus: async () => {
    try {
      set({ status: await api.getStatus() })
    } catch { /* the dashboard keeps its last known status */ }
  },
  refreshMetrics: async () => {
    try {
      set({ metrics: await api.getMetrics() })
    } catch { /* metrics are advisory */ }
  },
  refreshPlaylists: async () => {
    try {
      set({ playlists: await api.listPlaylists() })
    } catch (e) { get().reportError(e) }
  },
  refreshActivePlaylist: async () => {
    const id = get().activePlaylistId
    if (id == null) return set({ activePlaylist: null })
    try {
      set({ activePlaylist: await api.getPlaylist(id) })
    } catch (e) { get().reportError(e) }
  },
  refreshMedia: async () => {
    try {
      set({ media: await api.listMedia() })
    } catch (e) { get().reportError(e) }
  },
  refreshSchedules: async () => {
    try {
      set({ schedules: await api.listSchedules() })
    } catch (e) { get().reportError(e) }
  },
  refreshSettings: async () => {
    try {
      set({ settings: await api.getSettings() })
    } catch (e) { get().reportError(e) }
  },

  toast: (t) => {
    const id = toastSeq++
    set((s) => ({ toasts: [...s.toasts, { ...t, id }] }))
    setTimeout(() => get().dismissToast(id), t.kind === 'error' ? 12000 : 5000)
  },
  reportError: (e) => {
    const err = asLouverError(e)
    get().toast({
      kind: 'error',
      message: err.message,
      detail: err.detail ?? undefined,
      code: err.code_str,
    })
  },
  dismissToast: (id) => set((s) => ({ toasts: s.toasts.filter((t) => t.id !== id) })),
  dismissNotice: () => set({ startupNotice: null }),

  subscribe: async () => {
    const offs: Array<() => void> = []
    offs.push(await listen<RuntimeStatus>('louver://status', (status) => set({ status })))
    offs.push(
      await listen<NormalizeProgress>('louver://normalize', (p) =>
        set({ normalizing: p.percent >= 100 && p.remaining_files === 0 ? null : p }),
      ),
    )
    // One row finished analysis or preparation. The list redraws from the
    // database rather than from the event, so a refresh that arrives late
    // still shows the truth.
    offs.push(
      await listen<{ media_id: number }>('louver://media', () => {
        void get().refreshMedia()
        void get().refreshActivePlaylist()
      }),
    )
    offs.push(
      await listen<string>('louver://navigate', (page) => set({ page: page as Page })),
    )
    return () => offs.forEach((o) => o())
  },
}))
