/**
 * An in-memory stand-in for the Rust IPC surface.
 *
 * It backs `npm run dev` in a plain browser and the UI e2e suite (§58). It
 * models the behaviour the UI depends on — media status transitions, playlist
 * ordering, schedule validation, preflight outcomes and broadcast state — but
 * it is *not* a second implementation of the product: the real logic is tested
 * against real FFmpeg in the Rust suites.
 */
import { emitLocal } from '@/services/ipc'
import type {
  DashboardMetrics, DiskEstimate, ImportResult, LicenseState, Media, Playlist,
  PlaylistItemView, PlaylistView, PreflightReport, RuntimeStatus, ScheduleView,
  SettingsView, StreamEvent, StreamState, MetadataApplyState, MetadataOutcome,
} from '@/types'

export interface MockOptions {
  /** Files the picker returns. */
  filesToPick?: string[]
  hasStreamKey?: boolean
  licenseValid?: boolean
  /** Persisted settings carried across a simulated restart. */
  seedSettings?: Record<string, string>
  /** Simulate a machine where no browser can be opened. */
  failOpener?: boolean
  /** Report new schedules as being inside their window right now. */
  scheduleActiveNow?: boolean
  /**
   * YouTube answers 200 but keeps its own title — the reported failure, and
   * the only thing read-back catches.
   */
  youtubeIgnoresTitle?: boolean
  /** The day's free YouTube allowance is gone. */
  youtubeQuotaExhausted?: boolean
  /**
   * Make every YouTube Data API call fail. Used to check that the optional
   * half can break without taking the broadcast with it.
   */
  youtubeApiFails?: boolean
}

export function createMockBackend(opts: MockOptions = {}) {
  const youtube = {
    connected: false,
    applyOnStart: true,
    secretFallback: false,
    authDiagnostic: null as string | null,
    nextPresetId: 1,
    nextMessageId: 1,
    metadata: {
      title: '',
      description: '',
      tags: [] as string[],
      category_id: '10',
      privacy: 'unlisted' as const,
    },
    applied: null as null | Record<string, unknown>,
    applyState: { stage: 'off' } as MetadataApplyState,
    scheduleHolds: true,
    presets: [] as { id: number; name: string; title: string; description: string; tags: string[]; category_id: string; privacy: string }[],
    messages: [] as { id: number; position: number; text: string; enabled: boolean }[],
    chat: {
      enabled: false,
      order: 'sequential' as const,
      interval_secs: 1200,
      send_on_start: true,
      send_on_end: false,
      avoid_repeats: true,
    },
    chatStatus: {
      state: 'IDLE' as const,
      state_label: '꺼짐',
      live_chat_id_known: false,
      messages_sent: 0,
      seconds_until_next: null,
      last_error: null,
      last_error_code: null,
      broadcast_id: null,
      broadcast_title: null,
      broadcast_privacy: null,
    },
  }

  const settings = new Map<string, string>(Object.entries(opts.seedSettings ?? {}))
  let streamKey: string | null = opts.hasStreamKey === false ? null : 'abcd-efgh-ijkl-mnop'
  if (opts.hasStreamKey === false) streamKey = null

  const media: Media[] = []
  const playlists: Playlist[] = []
  const items: (Omit<PlaylistItemView, 'media'>)[] = []
  const schedules: ScheduleView[] = []
  const events: StreamEvent[] = []
  const ids = { media: 1, playlist: 1, item: 1, schedule: 1, event: 1 }

  let status: RuntimeStatus = idleStatus()
  let elapsedTimer: ReturnType<typeof setInterval> | null = null

  function idleStatus(): RuntimeStatus {
    return {
      supervisor: {
        state: 'IDLE',
        mode: (settings.get('stream_mode') as 'stream_copy') ?? 'stream_copy',
        pid: null,
        restart_count: 0,
        reconnect_count: 0,
        seconds_since_data: null,
        progress: { frames: 0, fps: 0, bitrate_kbps: 0, total_bytes: 0, out_time_ms: 0, speed: 0 },
        last_error: null,
        next_retry_in_secs: null,
      },
      playlist_id: null,
      playlist_name: null,
      current_item: null,
      next_item: null,
      current_index: null,
      item_count: 0,
      elapsed_secs: 0,
      remaining_secs: null,
      scheduled_end: null,
      start_reason: null,
      dry_run: false,
      next_scheduled_start: nextScheduledLabel(),
      cycle_duration_secs: 0,
    }
  }

  function nextScheduledLabel(): string | null {
    const s = schedules.find((x) => x.enabled)
    return s ? `${s.start_time} 예정` : null
  }

  function setStatus(next: Partial<RuntimeStatus>, sup: Partial<RuntimeStatus['supervisor']> = {}) {
    status = { ...status, ...next, supervisor: { ...status.supervisor, ...sup } }
    emitLocal('louver://status', status)
  }

  function log(level: StreamEvent['level'], message: string, code?: string) {
    // Mirrors the Rust masking guarantee so the UI is never handed a key.
    const masked = message.replace(/[A-Za-z0-9]{4}(-[A-Za-z0-9]{4}){3,}/g, '••••••••')
    events.unshift({ id: ids.event++, session_id: null, at: new Date().toISOString(), level, code: code ?? null, message: masked })
  }

  function itemsFor(playlistId: number): PlaylistItemView[] {
    return items
      .filter((i) => i.playlist_id === playlistId)
      .sort((a, b) => a.position - b.position)
      .map((i) => ({ ...i, media: media.find((m) => m.id === i.media_id)! }))
      .filter((i) => i.media)
  }

  function durationOf(m: Media) {
    return m.normalized_duration_secs ?? m.duration_secs
  }

  /** The apply, as Google reports it back afterwards. */
  function applyOutcome(ignoresTitle: boolean): MetadataOutcome {
    const m = youtube.metadata
    const actualTitle = ignoresTitle ? 'Playlist' : m.title
    const ok = (actual: string, applied = true) => ({ applied, actual })
    return {
      broadcast: {
        id: 'bcast-1',
        title: actualTitle,
        privacy: m.privacy,
        active_live_chat_id: 'chat-1',
        life_cycle_status: 'live',
      },
      verification: {
        title: { applied: actualTitle === m.title, actual: actualTitle },
        description: ok(m.description),
        tags: ok(m.tags.join(', ')),
        category: ok(m.category_id),
        privacy: ok(m.privacy),
      },
    }
  }

  function startBroadcast(playlistId: number, dryRun: boolean) {
    const list = itemsFor(playlistId).filter((i) => i.enabled)
    const pl = playlists.find((p) => p.id === playlistId)
    const cycle = list.reduce((a, i) => a + durationOf(i.media), 0)
    setStatus(
      {
        playlist_id: playlistId,
        playlist_name: pl?.name ?? null,
        item_count: list.length,
        current_item: list[0]?.media.display_name ?? null,
        next_item: list[1]?.media.display_name ?? list[0]?.media.display_name ?? null,
        current_index: 0,
        elapsed_secs: 0,
        dry_run: dryRun,
        start_reason: 'manual',
        cycle_duration_secs: cycle,
      },
      { state: 'CONNECTING', pid: 4242 },
    )
    log('info', `방송 시작: ${pl?.name ?? playlistId}`)
    setTimeout(() => {
      if (status.supervisor.state === 'CONNECTING') {
        setStatus({}, { state: 'LIVE', progress: { ...status.supervisor.progress, bitrate_kbps: 10216, frames: 30 } })
        log('info', '방송이 시작되었습니다')
      }
    }, 20)
    elapsedTimer = setInterval(() => {
      setStatus({ elapsed_secs: status.elapsed_secs + 1 })
    }, 1000)
  }

  function stopBroadcast() {
    if (elapsedTimer) clearInterval(elapsedTimer)
    elapsedTimer = null
    log('info', '방송을 종료했습니다 (사용자 요청)')
    status = { ...idleStatus(), supervisor: { ...idleStatus().supervisor, state: 'STOPPED' as StreamState } }
    emitLocal('louver://status', status)
  }

  function preflight(playlistId: number, dryRun: boolean): PreflightReport {
    const pl = playlists.find((p) => p.id === playlistId)
    const list = itemsFor(playlistId).filter((i) => i.enabled)
    const unready = list.filter((i) => i.media.status !== 'normalized' && i.media.status !== 'compatible')
    const checks: PreflightReport['checks'] = [
      pl && list.length
        ? { id: 'playlist', label: '플레이리스트', outcome: 'pass', detail: `${list.length}개 영상`, code: null }
        : { id: 'playlist', label: '플레이리스트', outcome: 'fail', detail: '플레이리스트가 비어 있습니다', code: 'LL-STREAM-004' },
      { id: 'files', label: '파일 확인', outcome: 'pass', detail: '모든 파일이 존재합니다', code: null },
      unready.length === 0
        ? { id: 'normalized', label: '방송 규격', outcome: 'pass', detail: '모든 영상이 방송 규격입니다', code: null }
        : { id: 'normalized', label: '방송 규격', outcome: 'fail', detail: `${unready.length}개 영상의 최적화가 필요합니다`, code: 'LL-STREAM-003' },
      dryRun || streamKey
        ? { id: 'stream_key', label: '스트림 키', outcome: 'pass', detail: dryRun ? '테스트 모드에서는 필요하지 않습니다' : '저장된 스트림 키를 사용합니다', code: null }
        : { id: 'stream_key', label: '스트림 키', outcome: 'fail', detail: '스트림 키가 없습니다', code: 'LL-STREAM-007' },
      { id: 'internet', label: '인터넷 연결', outcome: 'pass', detail: '연결됨', code: null },
      { id: 'ffmpeg', label: '방송 엔진', outcome: 'pass', detail: 'ffmpeg version 6.1.1', code: null },
      { id: 'ingest', label: '업로드 네트워크', outcome: 'pass', detail: 'a.rtmps.youtube.com:443 연결 가능', code: null },
    ]
    if (!dryRun && opts.licenseValid === false) {
      checks.push({ id: 'license', label: '라이선스', outcome: 'fail', detail: '라이선스가 없습니다', code: 'LL-LICENSE-001' })
    }
    return { checks, can_broadcast: !checks.some((c) => c.outcome === 'fail') }
  }

  function licenseState(): LicenseState {
    return opts.licenseValid === false
      ? { status: 'missing', payload: null, message: '라이선스가 없습니다.', device_binding_enforced: false }
      : { status: 'development', payload: null, message: '개발용 라이선스 (디버그 빌드 전용)', device_binding_enforced: false }
  }

  function settingsView(): SettingsView {
    const flag = (k: string, d = 'false') => (settings.get(k) ?? d) === 'true'
    return {
      rtmps_url: settings.get('rtmps_url') ?? 'rtmps://a.rtmps.youtube.com/live2',
      output_profile: settings.get('output_profile') ?? '1080p30',
      stream_mode: (settings.get('stream_mode') as SettingsView['stream_mode']) ?? 'stream_copy',
      launch_at_startup: flag('launch_at_startup'),
      start_minimized: flag('start_minimized'),
      minimize_to_tray: flag('minimize_to_tray', 'true'),
      auto_reconnect: flag('auto_reconnect', 'true'),
      developer_mode: flag('developer_mode'),
      enforce_device_binding: flag('enforce_device_binding'),
      first_run_complete: flag('first_run_complete'),
      active_playlist: settings.has('active_playlist') ? Number(settings.get('active_playlist')) : null,
      cache_location: '/data/LouverLive/cache',
      cache_size_bytes: 1024 * 1024 * 512,
      cache_size_label: '512.0 MB',
      stream_key_masked: streamKey ? '•'.repeat(12) : '',
      stream_key_hint: streamKey ? `••••${streamKey.slice(-4)}` : null,
      has_stream_key: !!streamKey,
      secret_backend: 'macOS 키체인',
      secret_backend_is_secure: true,
      ffmpeg_path: '/opt/louver/ffmpeg',
      ffmpeg_version: 'ffmpeg version 6.1.1',
      hardware_encoder: 'libx264',
      logs_dir: '/data/LouverLive/logs',
      app_version: '1.0.0',
      license: licenseState(),
      profiles: [
        { id: '1080p30', label: '1080p30 (권장)', video_kbps: 10000 },
        { id: '720p30', label: '720p30 (저대역폭)', video_kbps: 4000 },
      ],
    }
  }

  const handlers: Record<string, (a: Record<string, unknown>) => unknown> = {
    // --- media ---
    import_media: (a) => {
      const paths = a.paths as string[]
      const result: ImportResult = { imported: [], failed: [] }
      for (const p of paths) {
        const ext = p.split('.').pop()?.toLowerCase() ?? ''
        if (!['mp4', 'mov', 'mkv', 'm4v', 'webm', 'avi', 'ts'].includes(ext)) {
          result.failed.push({ path: p, code: 'LL-MEDIA-002', message: '지원하지 않는 영상 형식입니다.' })
          continue
        }
        const existing = media.find((m) => m.source_path === p)
        if (existing) { result.imported.push(existing); continue }
        // Sources are deliberately non-conforming so the optimize flow is exercised.
        const m: Media = {
          id: ids.media++,
          source_path: p,
          display_name: p.split(/[\\/]/).pop() ?? p,
          status: 'optimization_required',
          media_hash: `h${ids.media}`,
          normalized_path: null,
          normalized_profile: null,
          duration_secs: 3600 + ids.media * 7,
          normalized_duration_secs: null,
          width: 1280, height: 720, fps: 25,
          video_codec: 'h264', audio_codec: 'aac', pixel_format: 'yuv420p',
          is_hdr: false, file_size: 1024 * 1024 * 400,
          added_at: new Date().toISOString(), last_error: null,
        }
        media.push(m)
        result.imported.push(m)
      }
      return result
    },
    list_media: () => media,
    delete_media: (a) => { const i = media.findIndex((m) => m.id === a.id); if (i >= 0) media.splice(i, 1) },
    compatibility_reasons: () => ['해상도가 1920x1080이 아닙니다 (1280x720)', '프레임레이트가 30fps가 아닙니다 (25.00fps)'],
    estimate_optimization: (a): DiskEstimate => {
      const ids2 = a.mediaIds as number[]
      const targets = media.filter((m) => ids2.includes(m.id) && m.status === 'optimization_required')
      const total = targets.reduce((x, m) => x + m.duration_secs, 0)
      const estimated = Math.round(total * 1274000)
      return {
        files_to_process: targets.length,
        total_duration_secs: total,
        estimated_bytes: estimated,
        available_bytes: 184 * 1e9,
        has_enough_space: 184 * 1e9 >= estimated + 2 * 1024 ** 3,
        safety_margin_bytes: 2 * 1024 ** 3,
      }
    },
    optimize_media: (a) => {
      const ids2 = a.mediaIds as number[]
      let done = 0
      ids2.forEach((id, idx) => {
        const m = media.find((x) => x.id === id)
        if (!m) return
        m.status = 'normalized'
        m.normalized_path = `/cache/${m.media_hash}/normalized.mp4`
        m.normalized_profile = settings.get('output_profile') ?? '1080p30'
        m.normalized_duration_secs = Math.floor(m.duration_secs * 30) / 30
        done++
        emitLocal('louver://normalize', {
          media_id: id, file_name: m.display_name, percent: 100,
          files_done: idx + 1, files_total: ids2.length,
          remaining_files: ids2.length - idx - 1, estimated_cache_bytes: 0,
        })
      })
      return done
    },
    cancel_optimization: () => undefined,

    // --- playlists ---
    list_playlists: () => playlists,
    get_playlist: (a): PlaylistView | null => {
      const p = playlists.find((x) => x.id === a.id)
      if (!p) return null
      const list = itemsFor(p.id)
      const total = list.filter((i) => i.enabled).reduce((x, i) => x + durationOf(i.media), 0)
      const h = Math.floor(total / 3600), mm = Math.floor((total % 3600) / 60), ss = Math.floor(total % 60)
      return {
        playlist: p,
        items: list,
        total_duration_secs: total,
        total_duration_label: h > 0
          ? `${h}시간 ${String(mm).padStart(2, '0')}분 ${String(ss).padStart(2, '0')}초`
          : `${mm}분 ${String(ss).padStart(2, '0')}초`,
        unready_count: list.filter((i) => i.enabled && i.media.status !== 'normalized' && i.media.status !== 'compatible').length,
      }
    },
    create_playlist: (a) => {
      const p: Playlist = {
        id: ids.playlist++, name: a.name as string,
        playback_mode: 'sequential', output_profile: settings.get('output_profile') ?? '1080p30',
        created_at: new Date().toISOString(), updated_at: new Date().toISOString(),
      }
      playlists.push(p)
      return p.id
    },
    update_playlist: (a) => {
      const p = playlists.find((x) => x.id === a.id)
      if (p) {
        p.name = a.name as string
        p.playback_mode = a.playbackMode as Playlist['playback_mode']
        p.output_profile = a.outputProfile as string
      }
    },
    delete_playlist: (a) => {
      const i = playlists.findIndex((x) => x.id === a.id)
      if (i >= 0) playlists.splice(i, 1)
      for (let k = items.length - 1; k >= 0; k--) if (items[k]!.playlist_id === a.id) items.splice(k, 1)
    },
    add_to_playlist: (a) => {
      const pid = a.playlistId as number
      for (const mid of a.mediaIds as number[]) {
        const pos = items.filter((i) => i.playlist_id === pid).length
        items.push({ id: ids.item++, playlist_id: pid, media_id: mid, position: pos, enabled: true })
      }
    },
    reorder_playlist: (a) => {
      const order = a.itemIds as number[]
      order.forEach((id, idx) => {
        const it = items.find((i) => i.id === id)
        if (it) it.position = idx
      })
    },
    set_item_enabled: (a) => {
      const it = items.find((i) => i.id === a.itemId)
      if (it) it.enabled = a.enabled as boolean
    },
    remove_playlist_item: (a) => {
      const i = items.findIndex((x) => x.id === a.itemId)
      if (i >= 0) items.splice(i, 1)
    },

    // --- schedules ---
    list_schedules: () => schedules,
    create_schedule: (a) => {
      const start = a.startTime as string, end = a.endTime as string
      const days = a.daysOfWeek as number
      if (!days) throw { code_str: 'LL-SCHED-002', message: '반복할 요일을 하나 이상 선택해주세요.' }
      if (start === end) throw { code_str: 'LL-SCHED-001', message: '예약 시간이 올바르지 않습니다.' }
      const overnight = end <= start
      const labels = ['월', '화', '수', '목', '금', '토', '일']
      const s: ScheduleView = {
        id: ids.schedule++, playlist_id: a.playlistId as number,
        days_of_week: days, start_time: start, end_time: end, enabled: true,
        playlist_name: playlists.find((p) => p.id === a.playlistId)?.name ?? null,
        days_label: days === 0b1111111 ? '매일' : days === 0b0011111 ? '월~금'
          : labels.filter((_, i) => days & (1 << i)).join(' '),
        crosses_midnight: overnight,
        next_start: `내일 ${start}`, next_end: `${overnight ? '모레' : '내일'} ${end}`,
        window_duration_label: '12시간 00분 00초',
        // The fake clock is not inside any window unless a test says so.
        active_now: Boolean(opts.scheduleActiveNow),
        active_until: opts.scheduleActiveNow ? `오늘 ${end}` : null,
        playlist_missing: !playlists.some((p) => p.id === a.playlistId),
        playlist_ready_count: items.filter((i) => i.playlist_id === a.playlistId).length,
      }
      schedules.push(s)
      return s.id
    },
    update_schedule: (a) => {
      const s = schedules.find((x) => x.id === a.id)
      if (s) {
        s.enabled = a.enabled as boolean
        s.start_time = a.startTime as string
        s.end_time = a.endTime as string
        s.days_of_week = a.daysOfWeek as number
      }
    },
    delete_schedule: (a) => {
      const i = schedules.findIndex((x) => x.id === a.id)
      if (i >= 0) schedules.splice(i, 1)
    },

    // --- streaming ---
    get_status: () => status,
    run_preflight: (a) => preflight(a.playlistId as number, a.dryRun as boolean),
    start_broadcast: (a) => {
      const r = preflight(a.playlistId as number, false)
      if (!r.can_broadcast) {
        const f = r.checks.find((c) => c.outcome === 'fail')!
        throw { code_str: f.code, message: f.detail }
      }
      // The real backend runs the YouTube work here, before FFmpeg, and
      // refuses the start if it cannot be done (§B-4, §B-9).
      if (!a.skipYoutube) {
        const wanted = youtube.applyOnStart && youtube.metadata.title.trim() !== ''
        if (wanted && opts.youtubeQuotaExhausted) {
          // Not a refusal: the allowance is not something the user can fix,
          // and a 24/7 channel does not come off air over a title.
          youtube.applyState = { stage: 'quota_exhausted', requested: { ...youtube.metadata } }
          startBroadcast(a.playlistId as number, false)
          return status
        }
        if (wanted && !youtube.connected) {
          youtube.applyState = {
            stage: 'not_connected',
            requested: { ...youtube.metadata },
          }
          throw {
            code_str: 'LL-YOUTUBE-001',
            message: 'YouTube 계정이 연결되지 않았습니다. 설정에서 연결해주세요.',
            detail: '방송 설정 자동 적용을 사용하려면 YouTube 계정 연결이 필요합니다.',
          }
        }
        if (wanted) {
          if (opts.youtubeApiFails) {
            youtube.applyState = {
              stage: 'failed',
              requested: { ...youtube.metadata },
              error: {
                code_str: 'LL-YOUTUBE-004',
                message: 'YouTube에 연결하지 못했습니다. 잠시 후 다시 시도합니다.',
              },
            }
            throw {
              code_str: 'LL-YOUTUBE-004',
              message: 'YouTube에 연결하지 못했습니다. 잠시 후 다시 시도합니다.',
              detail: '방송 설정을 YouTube에 적용하지 못했습니다.',
            }
          }
          const outcome = applyOutcome(opts.youtubeIgnoresTitle === true)
          youtube.applyState = {
            stage: outcome.verification.title.applied ? 'applied' : 'mismatch',
            broadcast_id: outcome.broadcast.id,
            requested: { ...youtube.metadata },
            verification: outcome.verification,
          }
        }
      } else if (youtube.applyOnStart && youtube.metadata.title.trim() !== '') {
        // The user was shown the choice and took it, which is a different
        // thing from never having asked for an apply.
        youtube.applyState = { stage: 'skipped', requested: { ...youtube.metadata } }
      }
      startBroadcast(a.playlistId as number, false)
      return status
    },
    start_dry_run: (a) => {
      const r = preflight(a.playlistId as number, true)
      if (!r.can_broadcast) {
        const f = r.checks.find((c) => c.outcome === 'fail')!
        throw { code_str: f.code, message: f.detail }
      }
      startBroadcast(a.playlistId as number, true)
      return status
    },
    stop_broadcast: () => { stopBroadcast(); return status },
    stream_diagnostics: () => {
      const live = status.supervisor.state === 'LIVE'
      const copy = (settings.get('stream_mode') ?? 'stream_copy') === 'stream_copy'
      return {
        state: status.supervisor.state,
        configured_mode: copy ? 'stream_copy' : 'compatibility_encode',
        argv_is_stream_copy: copy,
        mismatch: null,
        video_encoder_args: copy ? [] : ['-c:v', '-b:v'],
        masked_command: live
          ? ['-re', '-stream_loop', '-1', '-f', 'concat', '-i', 'manifest.txt',
             ...(copy ? ['-c', 'copy'] : ['-c:v', 'libx264']),
             '-f', 'flv', 'rtmps://a.rtmps.youtube.com/live2/••••••••']
          : [],
        ffmpeg_pid: live ? 4242 : null,
        ffmpeg_cpu_percent: live && copy ? 1.6 : live ? 61.2 : 0,
        verdict: !live
          ? '방송 중이 아닙니다'
          : copy
            ? 'STREAM COPY: 영상 재인코딩 없음 (CPU 사용량이 낮아야 정상입니다)'
            : 'COMPATIBILITY MODE: 실시간 재인코딩 중 (CPU 사용량이 높습니다)',
        ffmpeg_memory_bytes: live ? 63 * 1024 * 1024 : 0,
        reconnect_count: status.supervisor.reconnect_count,
        seconds_since_progress: live ? 0 : null,
        publishing: live,
        bytes_sent: live ? 1_400_000_000 : 0,
        local_test_sink: status.dry_run
          ? { kind: 'Rtmp', target: 'rtmp://127.0.0.1:1935/live/louver-test' }
          : { kind: 'None' },
      }
    },
    stream_mode_label: () => (settings.get('stream_mode') === 'compatibility_encode' ? 'COMPATIBILITY ENCODE' : 'STREAM COPY'),
    simulate_ffmpeg_crash: () => {
      setStatus({}, { state: 'RECONNECTING', reconnect_count: status.supervisor.reconnect_count + 1, next_retry_in_secs: 2 })
      log('warn', '방송이 중단되었습니다. 2초 후 자동으로 다시 연결합니다')
      setTimeout(() => setStatus({}, { state: 'LIVE', next_retry_in_secs: null }), 30)
    },

    // --- settings ---
    get_settings: () => settingsView(),
    set_setting: (a) => { settings.set(a.key as string, a.value as string) },
    set_stream_key: (a) => {
      const k = (a.key as string).trim()
      if (!k) throw { code_str: 'LL-STREAM-007', message: '스트림 키가 없습니다.' }
      if (/[\s/\\]/.test(k)) throw { code_str: 'LL-CONFIG-001', message: '설정 값이 올바르지 않습니다.' }
      streamKey = k
    },
    reveal_stream_key: () => {
      if (!streamKey) throw { code_str: 'LL-SEC-002', message: '저장된 스트림 키가 없습니다.' }
      return streamKey
    },
    clear_stream_key: () => { streamKey = null },
    get_license: () => licenseState(),
    install_license: () => licenseState(),

    // --- youtube (V2) ---
    youtube_status: () => ({
      connected: youtube.connected,
      channel_id: youtube.connected ? 'UC-room' : null,
      channel_title: youtube.connected ? 'ROOM.' : null,
      has_credentials: true,
      apply_on_start: youtube.applyOnStart,
      using_custom_client: false,
      secret_fallback_enabled: youtube.secretFallback,
      last_auth_diagnostic: youtube.authDiagnostic,
      client_id_hint: '…googleusercontent.com',
      secret_backend: 'macOS 키체인',
      secret_backend_is_secure: true,
      connecting_error: null,
    }),
    youtube_set_credentials: () => backend('youtube_status', {}),
    youtube_switch_account: () => {
      youtube.connected = true
      return 'https://accounts.google.com/o/oauth2/v2/auth?mock=1&prompt=select_account'
    },
    youtube_begin_connect: () => {
      // The real flow opens a browser; the mock connects immediately so the
      // UI path after consent can be driven.
      youtube.connected = true
      return 'https://accounts.google.com/o/oauth2/v2/auth?mock=1'
    },
    youtube_set_secret_fallback: (a: Record<string, unknown>) => {
      youtube.secretFallback = Boolean(a.enabled)
      return backend('youtube_status', {})
    },
    youtube_disconnect: () => {
      youtube.connected = false
      return backend('youtube_status', {})
    },
    youtube_get_metadata: () => youtube.metadata,
    youtube_save_metadata: (a: Record<string, unknown>) => {
      youtube.metadata = a.metadata as typeof youtube.metadata
      return youtube.metadata
    },
    youtube_apply_metadata: () => {
      if (opts.youtubeApiFails) {
        throw {
          code_str: 'LL-YOUTUBE-004',
          message: 'YouTube에 연결하지 못했습니다. 방송 송출은 계속됩니다.',
        }
      }
      youtube.applied = { ...youtube.metadata }
      const outcome = applyOutcome(opts.youtubeIgnoresTitle === true)
      youtube.applyState = {
        stage: outcome.verification.title.applied ? 'applied' : 'mismatch',
        broadcast_id: outcome.broadcast.id,
        requested: { ...youtube.metadata },
        verification: outcome.verification,
        error: null,
      }
      return outcome
    },
    youtube_set_apply_on_start: (a: Record<string, unknown>) => {
      youtube.applyOnStart = Boolean(a.enabled)
      return undefined
    },
    youtube_apply_state: () => youtube.applyState,
    youtube_quota: () => ({
      used_percent: opts.youtubeQuotaExhausted ? 100 : 37,
      spent: opts.youtubeQuotaExhausted ? 9500 : 3500,
      cap: 10000,
      exhausted: Boolean(opts.youtubeQuotaExhausted),
      day: '2026-09-20',
      buckets: [
        { key: 'search.list', calls: 0, daily_calls: 100, exhausted: false },
        { key: 'videos.insert', calls: 0, daily_calls: 100, exhausted: false },
      ],
    }),
    youtube_schedule_holds: () => youtube.scheduleHolds,
    youtube_set_schedule_holds: (a: Record<string, unknown>) => {
      youtube.scheduleHolds = Boolean(a.holds)
    },
    youtube_apply_plan: () => ({
      wanted: youtube.applyOnStart && youtube.metadata.title.trim() !== '',
      connected: youtube.connected,
      chat_enabled: youtube.chat.enabled,
    }),
    youtube_current_broadcast: () => ({
      id: 'bcast-1',
      title: youtube.metadata.title,
      privacy: youtube.metadata.privacy,
      active_live_chat_id: 'chat-1',
      life_cycle_status: 'live',
    }),
    youtube_list_presets: () => youtube.presets,
    youtube_save_preset: (a: Record<string, unknown>) => {
      const name = String(a.name)
      const metadata = a.metadata as typeof youtube.metadata
      youtube.presets = [
        ...youtube.presets.filter((p) => p.name !== name),
        { id: youtube.nextPresetId++, name, ...metadata },
      ]
      return youtube.presets
    },
    youtube_delete_preset: (a: Record<string, unknown>) => {
      youtube.presets = youtube.presets.filter((p) => p.id !== Number(a.id))
      return youtube.presets
    },
    chat_list_messages: () => youtube.messages,
    chat_add_message: (a: Record<string, unknown>) => {
      youtube.messages = [
        ...youtube.messages,
        { id: youtube.nextMessageId++, position: youtube.messages.length, text: String(a.text), enabled: true },
      ]
      return youtube.messages
    },
    chat_update_message: (a: Record<string, unknown>) => {
      youtube.messages = youtube.messages.map((m) =>
        m.id === Number(a.id) ? { ...m, text: String(a.text), enabled: Boolean(a.enabled) } : m,
      )
      return youtube.messages
    },
    chat_delete_message: (a: Record<string, unknown>) => {
      youtube.messages = youtube.messages.filter((m) => m.id !== Number(a.id))
      return youtube.messages
    },
    chat_reorder_messages: (a: Record<string, unknown>) => {
      const ids = a.ids as number[]
      youtube.messages = ids
        .map((id, i) => {
          const m = youtube.messages.find((x) => x.id === id)!
          return { ...m, position: i }
        })
        .filter(Boolean)
      return youtube.messages
    },
    chat_get_settings: () => youtube.chat,
    chat_save_settings: (a: Record<string, unknown>) => {
      const next = a.settings as typeof youtube.chat
      if (next.interval_secs < 300) throw { code_str: 'LL-CONFIG-001', message: '전송 간격은 최소 5분입니다' }
      youtube.chat = next
      return youtube.chat
    },
    chat_status: () => youtube.chatStatus,
    chat_min_interval_secs: () => 300,

    // --- system ---
    get_metrics: (): DashboardMetrics => ({
      app_cpu_percent: 1.2,
      app_memory_bytes: 326 * 1024 * 1024,
      system_cpu_percent: 11,
      total_memory_bytes: 16 * 1024 ** 3,
      available_memory_bytes: 9 * 1024 ** 3,
      ffmpeg_cpu_percent: status.supervisor.state === 'LIVE' ? 4.1 : 0,
      ffmpeg_memory_bytes: status.supervisor.state === 'LIVE' ? 88 * 1024 * 1024 : 0,
      app_memory_label: '326.0 MB',
      cache_bytes: 512 * 1024 * 1024,
      cache_label: '512.0 MB',
      free_disk_bytes: 184 * 1e9,
      free_disk_label: '171.4 GB',
      sleep_prevented: status.supervisor.state === 'LIVE' && !status.dry_run,
      ffmpeg_ready: true,
      ffprobe_ready: true,
      stream_mode_label: (settings.get('stream_mode') ?? 'stream_copy') === 'stream_copy'
        ? 'STREAM COPY'
        : 'COMPATIBILITY ENCODE',
      license_label: '개발용',
      license_is_development: true,
    }),
    recent_events: () => events.slice(0, 100),
    read_log: () => ['2026-03-02 20:00:00.000 [INFO] 방송 시작: Night Jazz', '2026-03-02 20:00:01.120 [INFO] 방송이 시작되었습니다'],
    logs_directory: () => '/data/LouverLive/logs',
    clear_cache: () => {
      media.forEach((m) => {
        if (m.normalized_path) { m.status = 'optimization_required'; m.normalized_path = null }
      })
      return '512.0 MB'
    },
    cache_in_use_count: () => media.filter((m) => m.normalized_path).length,
    take_startup_notice: () => null,
    uptime_warnings: () => [
      '방송 중에는 컴퓨터와 인터넷 연결이 유지되어야 합니다.',
      '노트북의 경우 덮개를 닫으면 방송이 중단될 수 있습니다.',
      '안정적인 라이브를 위해 유선 인터넷 연결을 권장합니다.',
    ],

    // --- picker stand-ins ---
    __pick_files: () => opts.filesToPick ?? ['/videos/night01.mp4', '/videos/night02.mp4', '/videos/night03.mp4'],
    __pick_license: () => '/tmp/license.json',
    __reveal: () => undefined,
    __open_url: () => {
      // A machine with no browser, or a denied capability, is a real case: the
      // UI has to show the address rather than swallow the attempt.
      if (opts.failOpener) throw new Error('no handler for opening a URL')
      return undefined
    },
  }

  const backend = (cmd: string, args: Record<string, unknown>) => {
    const h = handlers[cmd]
    if (!h) throw new Error(`mock backend: unknown command "${cmd}"`)
    return h(args)
  }

  /** Settings survive a simulated restart, which the e2e suite checks. */
  backend.snapshotSettings = () => Object.fromEntries(settings)
  backend.stop = () => { if (elapsedTimer) clearInterval(elapsedTimer) }
  return backend
}
