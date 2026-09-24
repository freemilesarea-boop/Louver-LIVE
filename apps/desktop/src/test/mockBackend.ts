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
  DashboardMetrics, ImportResult, Media, Playlist,
  PlaylistItemView, PlaylistView, PreflightReport, RuntimeStatus, ScheduleView,
  SettingsView, StreamEvent, StreamState, MetadataApplyState, MetadataOutcome,
  ProvisionStep, StepRecord, LouverError,
} from '@/types'

export interface MockOptions {
  /** Files the picker returns. */
  filesToPick?: string[]
  hasStreamKey?: boolean
  /** Persisted settings carried across a simulated restart. */
  seedSettings?: Record<string, string>
  /** Simulate a machine where no browser can be opened. */
  failOpener?: boolean
  /**
   * Background analysis is queued but never runs.
   *
   * Stands for the real case that started all this: a 90-minute source whose
   * preparation takes 26 minutes. Nothing about drawing the playlist may wait
   * on it.
   */
  preparationStalls?: boolean
  /**
   * Preparation leaves the files unready, as a cancelled or failed run does.
   *
   * The only way the library holds a file that is not broadcastable: adding a
   * video prepares it, so this is the state the retry card exists for.
   */
  preparationFails?: boolean
  /** Report new schedules as being inside their window right now. */
  scheduleActiveNow?: boolean
  /**
   * YouTube answers 200 but keeps its own title — the reported failure, and
   * the only thing read-back catches.
   */
  youtubeIgnoresTitle?: boolean
  /** The day's free YouTube allowance is gone. */
  youtubeQuotaExhausted?: boolean
  /** The scheduler is already watching the clock. */
  schedulerArmed?: boolean
  /** A scheduled window is open right now. */
  occurrenceOpen?: { start: string; end: string; phase: 'preparing' | 'live'; retryInSecs?: number }
  /**
   * Make every YouTube Data API call fail. Used to check that the optional
   * half can break without taking the broadcast with it.
   */
  youtubeApiFails?: boolean
  /** Which preparation step `youtubeApiFails` should fail at. */
  youtubeFailsAt?: ProvisionStep
}

export function createMockBackend(opts: MockOptions = {}) {
  const youtube = {
    connected: false,
    applyOnStart: true,
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
      active_occurrence: null,
      scheduler_state: 'STOPPED',
    }
  }

  let restoreOnLaunch = true

  function schedulerStatusView() {
    const s = schedules.find((x) => x.enabled)
    return {
      state: schedulerState(),
      armed,
      enabled_count: schedules.filter((x) => x.enabled).length,
      next_start: s ? `내일 ${s.start_time}` : null,
      next_end: s ? `내일 ${s.end_time}` : null,
      next_playlist: s?.playlist_name ?? null,
      seconds_until_start: s ? 74 : null,
      active_start: armed ? opts.occurrenceOpen?.start ?? null : null,
      active_end: armed ? opts.occurrenceOpen?.end ?? null : null,
      last_error: null,
      restore_on_launch: restoreOnLaunch,
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

  /**
   * A preparation that got as far as `step` and was refused there.
   *
   * The shape the real backend produces: the steps before it succeeded, the
   * failure carries Google's own words, and the stage is what the user reads.
   */
  function failureAt(step: ProvisionStep): {
    error: LouverError
    stage: string
    remedy: string
    steps: StepRecord[]
  } {
    const sequence: ProvisionStep[] = [
      'token_refresh', 'broadcast_list', 'broadcast_insert', 'stream_list',
      'broadcast_bind', 'metadata_apply',
    ]
    const at = sequence.indexOf(step)
    const detail: Record<ProvisionStep, [string, string, string]> = {
      token_refresh: ['LL-YOUTUBE-AUTH-REFRESH', 'Google 인증 갱신 실패',
        'oauth2.token(refresh_token) HTTP 400 · invalid_grant: Token has been expired or revoked.'],
      broadcast_list: ['LL-YOUTUBE-004', '예약 방송 목록 조회 실패',
        'liveBroadcasts.list HTTP 400 reason=incompatibleParameters · Incompatible parameters specified in the request: broadcastStatus, mine'],
      broadcast_insert: ['LL-YOUTUBE-004', '예약 방송 생성 실패',
        'liveBroadcasts.insert HTTP 403 reason=liveStreamingNotEnabled · The user is not enabled for live streaming.'],
      stream_list: ['LL-YOUTUBE-004', '스트림 연결 실패',
        'liveStreams.list HTTP 401 reason=authError · Invalid Credentials'],
      broadcast_bind: ['LL-YOUTUBE-004', '스트림 연결 실패',
        'liveBroadcasts.bind HTTP 403 reason=insufficientPermissions · Request had insufficient authentication scopes.'],
      metadata_apply: ['LL-YOUTUBE-004', '방송 정보 적용 실패',
        'liveBroadcasts.update HTTP 400 reason=invalidTitle · Invalid title.'],
      stream_active: ['LL-YOUTUBE-004', '스트림 수신 확인 실패', 'liveStreams.list HTTP 503'],
      broadcast_transition: ['LL-YOUTUBE-004', 'LIVE 전환 실패',
        'liveBroadcasts.transition HTTP 403 reason=errorStreamInactive · The stream is not active.'],
    }
    const remedy: Record<ProvisionStep, string> = {
      token_refresh: '설정 → YouTube에서 계정을 다시 연결해주세요.',
      broadcast_list: 'YouTube 채널에서 실시간 스트리밍이 사용 설정되어 있는지 확인해주세요.',
      broadcast_insert: 'YouTube 채널에서 실시간 스트리밍이 사용 설정되어 있는지 확인해주세요.',
      stream_list: '설정에 입력한 스트림 키가 이 채널의 것인지 확인해주세요.',
      broadcast_bind: '설정에 입력한 스트림 키가 이 채널의 것인지 확인해주세요.',
      metadata_apply: '방송 설정의 제목·설명·태그를 확인해주세요.',
      stream_active: '인터넷 연결과 스트림 키를 확인해주세요.',
      broadcast_transition: '잠시 후 자동으로 다시 시도합니다.',
    }
    const stepMessage: Record<ProvisionStep, string> = {
      token_refresh: 'Google 인증을 갱신하지 못했습니다.',
      broadcast_list: '예약 방송 정보를 조회하지 못했습니다.',
      broadcast_insert: '예약 방송을 만들지 못했습니다.',
      stream_list: '스트림 키에 맞는 수신 지점을 찾지 못했습니다.',
      broadcast_bind: '방송을 스트림에 연결하지 못했습니다.',
      metadata_apply: '방송 정보를 적용하지 못했습니다.',
      stream_active: 'YouTube가 영상을 받고 있는지 확인하지 못했습니다.',
      broadcast_transition: '방송을 LIVE로 전환하지 못했습니다.',
    }
    const [code, stage, googleWords] = detail[step]
    const steps: StepRecord[] = sequence.slice(0, at < 0 ? 0 : at)
      .map((s) => ({ step: s, outcome: 'ok' as const, detail: null, error_code: null }))
    steps.push({ step, outcome: 'failed', detail: googleWords, error_code: code })
    return {
      error: {
        code_str: code,
        // The step's own sentence, the way the backend now reports it: a
        // refused parameter is not a lost connection, and telling the user
        // YouTube could not be reached sends them to check their internet.
        message: code === 'LL-YOUTUBE-AUTH-REFRESH'
          ? 'Google 인증 갱신에 실패했습니다. 설정에서 YouTube 계정을 다시 연결해주세요.'
          : stepMessage[step],
        detail: googleWords,
      },
      stage,
      remedy: remedy[step],
      steps,
    }
  }

  /** Is this computer watching the clock? Saving a rule does not set it. */
  let armed = Boolean(opts.schedulerArmed)

  function schedulerState(): RuntimeStatus['scheduler_state'] {
    if (!armed) return 'STOPPED'
    if (status.supervisor.state === 'LIVE' && status.start_reason === 'scheduled') return 'LIVE'
    if (opts.occurrenceOpen) return 'STARTING'
    return 'WAITING'
  }

  /** The open window, reported whatever the stream is doing. */
  function occurrence() {
    const o = opts.occurrenceOpen
    if (!o) return null
    return {
      start: o.start,
      end: o.end,
      playlist_id: playlists[0]?.id ?? 1,
      phase: status.supervisor.state === 'LIVE' ? 'live' as const : o.phase,
      retry_in_secs: o.retryInSecs ?? null,
      attempts: o.retryInSecs == null ? 0 : 1,
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
        : { id: 'normalized', label: '방송 규격', outcome: 'fail', detail: `${unready.length}개 영상의 방송 준비가 필요합니다`, code: 'LL-STREAM-003' },
      dryRun || streamKey
        ? { id: 'stream_key', label: '스트림 키', outcome: 'pass', detail: dryRun ? '테스트 모드에서는 필요하지 않습니다' : '저장된 스트림 키를 사용합니다', code: null }
        : { id: 'stream_key', label: '스트림 키', outcome: 'fail', detail: '스트림 키가 없습니다', code: 'LL-STREAM-007' },
      { id: 'internet', label: '인터넷 연결', outcome: 'pass', detail: '연결됨', code: null },
      { id: 'ffmpeg', label: '방송 엔진', outcome: 'pass', detail: 'ffmpeg version 6.1.1', code: null },
      { id: 'ingest', label: '업로드 네트워크', outcome: 'pass', detail: 'a.rtmps.youtube.com:443 연결 가능', code: null },
    ]
    return { checks, can_broadcast: !checks.some((c) => c.outcome === 'fail') }
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
      profiles: [
        { id: '1080p30', label: '1080p30 (권장)', video_kbps: 10000 },
        { id: '720p30', label: '720p30 (저대역폭)', video_kbps: 4000 },
      ],
    }
  }

  /**
   * Run work after the current call returns, the way a thread would.
   *
   * The real `add_media` spawns a thread and returns; the playlist is drawn
   * from what it returned, not from what the thread goes on to do. A mock that
   * did the work inline would hide exactly the bug this exists to prevent.
   */
  const backgroundTimers: Array<ReturnType<typeof setTimeout>> = []
  function queueBackground(fn: () => void) {
    backgroundTimers.push(setTimeout(fn, 0))
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
        const m: Media = {
          id: ids.media++,
          source_path: p,
          display_name: p.split(/[\\/]/).pop() ?? p,
          status: 'imported',
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
    prepare_media: (a) => {
      const ids2 = a.mediaIds as number[]
      // Stage 1: the probe, which fills in what the row did not know yet.
      for (const id of ids2) {
        const m = media.find((x) => x.id === id)
        if (!m) continue
        m.duration_secs = 3600 + id * 7
        m.width = 1280; m.height = 720; m.fps = 25
        m.video_codec = 'h264'; m.audio_codec = 'aac'; m.pixel_format = 'yuv420p'
        m.status = 'optimization_required'
        emitLocal('louver://media', { media_id: id })
      }
      if (opts.preparationFails) return 0
      let done = 0
      ids2.forEach((id, idx) => {
        const m = media.find((x) => x.id === id)
        if (!m) return
        m.status = 'normalized'
        m.normalized_path = `/cache/${m.media_hash}/normalized.mp4`
        m.normalized_profile = settings.get('output_profile') ?? '1080p30'
        m.normalized_duration_secs = Math.floor(m.duration_secs * 30) / 30
        done++
        emitLocal('louver://media', { media_id: id })
        emitLocal('louver://normalize', {
          media_id: id, file_name: m.display_name, percent: 100,
          files_done: idx + 1, files_total: ids2.length,
          remaining_files: ids2.length - idx - 1, estimated_cache_bytes: 0,
          mode_label: '방송에 맞게 준비 중', speed_x: 3.2, eta_secs: 42, engine_label: 'CPU',
        })
      })
      return done
    },
    cancel_optimization: () => undefined,
    /**
     * Register now, analyse later — the shape the real command has.
     *
     * The rows come back as `imported` and the background work happens on a
     * timer, so a test that asserts the playlist is drawn before preparation
     * finishes is asserting the thing the user complained about.
     */
    add_media: (a) => {
      const result = handlers.import_media!(a) as ImportResult
      const pending = result.imported.filter((m) => m.status === 'imported')
      if (pending.length && !opts.preparationStalls) {
        const ids = pending.map((m) => m.id)
        queueBackground(() => { handlers.prepare_media!({ mediaIds: ids }) })
      }
      return { ...result, analysing: pending.length }
    },

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
        unready_count: list.filter((i) => i.enabled && i.media.status !== 'normalized').length,
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
    scheduler_status: () => schedulerStatusView(),
    scheduler_arm: () => {
      if (!schedules.some((s) => s.enabled)) {
        throw { code_str: 'LL-SCHED-002', message: '사용 중인 예약이 없습니다. 예약을 먼저 추가하거나 켜주세요.' }
      }
      armed = true
      return schedulerStatusView()
    },
    scheduler_disarm: () => {
      armed = false
      return schedulerStatusView()
    },
    scheduler_set_restore: (a: Record<string, unknown>) => {
      restoreOnLaunch = Boolean(a.enabled)
    },
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
        // `occurrenceOpen` says the same thing from the runtime's side.
        active_now: Boolean(opts.scheduleActiveNow || opts.occurrenceOpen),
        active_until: opts.scheduleActiveNow || opts.occurrenceOpen ? `오늘 ${end}` : null,
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
    get_status: () => ({
      ...status,
      // Nothing is watched unless the scheduler is running, so neither is
      // reported — the same rule the runtime applies.
      active_occurrence: armed ? occurrence() : null,
      scheduler_state: schedulerState(),
    }),
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
            // Mirrors the real backend: the step that failed is named, the
            // ones before it are kept, and Google's own words ride in the
            // detail rather than being flattened into the message.
            const failure = failureAt(opts.youtubeFailsAt ?? 'metadata_apply')
            youtube.applyState = {
              stage: 'failed',
              requested: { ...youtube.metadata },
              error: failure.error,
              failed_stage: failure.stage,
              failed_remedy: failure.remedy,
              origin: 'manual',
              steps: failure.steps,
            }
            throw { ...failure.error }
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

    // --- youtube (V2) ---
    youtube_status: () => ({
      connected: youtube.connected,
      channel_id: youtube.connected ? 'UC-room' : null,
      channel_title: youtube.connected ? 'ROOM.' : null,
      has_credentials: true,
      apply_on_start: youtube.applyOnStart,
      using_custom_client: false,
      has_client_secret: true,
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
  backend.stop = () => {
    if (elapsedTimer) clearInterval(elapsedTimer)
    for (const t of backgroundTimers) clearTimeout(t)
    backgroundTimers.length = 0
  }
  return backend
}
