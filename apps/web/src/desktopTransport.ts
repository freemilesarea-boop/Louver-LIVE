/**
 * The same {@link Transport}, backed by the desktop app's Rust commands.
 *
 * This is what keeps the environment out of the components: the cloud screens
 * can render against the desktop's single-machine backend without a single
 * `if (isTauri())` anywhere above this file.
 *
 * What the desktop genuinely has is mapped. What it has no equivalent for —
 * accounts, plans, uploading a file to somewhere else — rejects with a message
 * that says so, rather than pretending.
 */
import { api, listen } from '@/services/ipc'
import type { Media, RuntimeStatus, StreamState } from '@/types'
import type {
  Broadcast, BroadcastEvent, CloudMedia, Dashboard, DesiredState, Health, Me, MediaState, Metrics,
  NewBroadcast, NewDestination, RuntimeState, StreamDestination, Subscription,
} from './cloud'
import type { Transport } from './transport'

const ONLY_ON_THE_WEB = (what: string) => {
  const e = new Error(`${what}은(는) 웹 버전에서만 사용할 수 있습니다.`)
  return Object.assign(e, { code_str: 'LL-DESKTOP', message: e.message })
}

/** The desktop is one machine with one stream key: one destination, always. */
export const LOCAL_DESTINATION: StreamDestination = {
  id: 'local',
  user_id: 'local',
  label: '이 컴퓨터의 스트림 키',
  rtmps_url: 'rtmps://a.rtmps.youtube.com/live2',
  key_masked: '••••••••••••',
  created_at: '',
}

export class DesktopTransport implements Transport {
  readonly kind = 'desktop' as const

  async register(): Promise<Me> {
    throw ONLY_ON_THE_WEB('계정 만들기')
  }

  async login(): Promise<Me> {
    throw ONLY_ON_THE_WEB('로그인')
  }

  async logout(): Promise<void> {
    /* nothing to end: the desktop app is the session */
  }

  async me(): Promise<Me> {
    return { id: 'local', email: '', plan_id: 'desktop' }
  }

  async subscription(): Promise<Subscription> {
    return {
      user_id: 'local',
      plan_id: 'desktop',
      plan_label: '데스크톱',
      status: 'active',
      // One machine sends one stream. The number is the truth here, not a plan.
      limits: { max_concurrent_streams: 1 },
    }
  }

  async listMedia(): Promise<CloudMedia[]> {
    return (await api.listMedia()).map(fromDesktopMedia)
  }

  async uploadMedia(): Promise<CloudMedia> {
    throw ONLY_ON_THE_WEB('영상 업로드')
  }

  async deleteMedia(id: string): Promise<void> {
    await api.deleteMedia(Number(id))
  }

  async listDestinations(): Promise<StreamDestination[]> {
    return [LOCAL_DESTINATION]
  }

  async createDestination(_input: NewDestination): Promise<StreamDestination> {
    throw ONLY_ON_THE_WEB('송출 대상 추가')
  }

  async deleteDestination(): Promise<void> {
    throw ONLY_ON_THE_WEB('송출 대상 삭제')
  }

  /**
   * Each playlist is a broadcast; the one that is streaming carries the live
   * state. The desktop runs one at a time, so `active` is 0 or 1.
   */
  async dashboard(): Promise<Dashboard> {
    const [playlists, status] = await Promise.all([api.listPlaylists(), api.getStatus()])
    const broadcasts = playlists.map((p) => toBroadcast(p.id, p.name, status))
    return {
      plan_label: '데스크톱',
      active: broadcasts.filter((b) => b.desired_state === 'running').length,
      allowed: 1,
      broadcasts,
    }
  }

  async createBroadcast(_input: NewBroadcast): Promise<Broadcast> {
    throw ONLY_ON_THE_WEB('방송 만들기')
  }

  async startBroadcast(id: string): Promise<Broadcast> {
    const status = await api.startBroadcast(Number(id))
    return toBroadcast(Number(id), status.playlist_name ?? '', status)
  }

  async stopBroadcast(id: string): Promise<Broadcast> {
    const status = await api.stopBroadcast()
    return toBroadcast(Number(id), status.playlist_name ?? '', status)
  }

  async restartBroadcast(id: string): Promise<Broadcast> {
    await api.stopBroadcast()
    return this.startBroadcast(id)
  }

  async deleteBroadcast(id: string): Promise<void> {
    await api.deletePlaylist(Number(id))
  }

  async logs(_id: string, limit = 100): Promise<BroadcastEvent[]> {
    const events = await api.recentEvents(limit)
    return events.map((e, i) => ({
      id: i,
      broadcast_id: _id,
      at: e.at,
      level: e.level,
      message: e.message,
    }))
  }

  /** The desktop is the machine in front of you. That is the whole point. */
  async health(): Promise<Health> {
    return {
      status: 'ok',
      version: '',
      deployment: 'local',
      checks: { api: true, database: true, ffmpeg: true, storage: true },
    }
  }

  async metrics(): Promise<Metrics> {
    const status = await api.getStatus()
    const m = await api.getMetrics()
    const d = await this.dashboard()
    return {
      deployment: 'local',
      server: {
        cpu_percent: m.system_cpu_percent,
        memory_total_bytes: m.total_memory_bytes,
        memory_available_bytes: m.available_memory_bytes,
        process_cpu_percent: m.app_cpu_percent,
        process_memory_bytes: m.app_memory_bytes,
        disk_available_bytes: m.free_disk_bytes,
        // The desktop sends from this machine and meters nothing: there is no
        // bill to attribute, which is the difference the label is warning about.
        egress_bytes: 0,
      },
      broadcasts: d.broadcasts.map((b) => ({
        id: b.id,
        name: b.name,
        runtime_state: b.runtime_state,
        uptime_secs: b.uptime_secs,
        bytes_sent: 0,
        average_bitrate_bps: 0,
        restart_count: b.restart_count,
        last_error: b.last_error ?? null,
        ffmpeg_pid: status.supervisor.pid ?? null,
        last_heartbeat: null,
      })),
    }
  }

  /** The desktop already pushes a status event; a snapshot follows each one. */
  watchDashboard(onSnapshot: (d: Dashboard) => void): () => void {
    let live = true
    const push = () => {
      if (!live) return
      this.dashboard()
        .then((d) => live && onSnapshot(d))
        .catch(() => undefined)
    }
    const unlisten = listen('status', push)
    const timer = setInterval(push, 3000)
    return () => {
      live = false
      clearInterval(timer)
      unlisten.then((un) => un()).catch(() => undefined)
    }
  }
}

const MEDIA_STATE: Record<string, MediaState> = {
  imported: 'analysing',
  compatible: 'preparing',
  optimization_required: 'preparing',
  normalized: 'ready',
  missing: 'failed',
  failed: 'failed',
}

function fromDesktopMedia(m: Media): CloudMedia {
  return {
    id: String(m.id),
    user_id: 'local',
    filename: m.display_name,
    size_bytes: m.file_size,
    state: MEDIA_STATE[m.status] ?? 'uploaded',
    duration_secs: m.duration_secs,
    width: m.width,
    height: m.height,
    fps: m.fps,
    video_codec: m.video_codec,
    audio_codec: m.audio_codec ?? null,
    container: '',
    bitrate_bps: 0,
    prepared_duration_secs: m.normalized_duration_secs ?? null,
    last_error: m.last_error ?? null,
    created_at: m.added_at,
  }
}

const RUNTIME: Record<StreamState, RuntimeState> = {
  IDLE: 'CREATED',
  PREPARING: 'PREPARING',
  CONNECTING: 'STARTING',
  LIVE: 'RUNNING',
  RECONNECTING: 'RECONNECTING',
  STOPPING: 'STOPPING',
  STOPPED: 'STOPPED',
  ERROR: 'FAILED',
}

function toBroadcast(playlistId: number, name: string, status: RuntimeStatus): Broadcast {
  const isThisOne = status.playlist_id === playlistId
  const runtime: RuntimeState = isThisOne ? RUNTIME[status.supervisor.state] : 'CREATED'
  const desired: DesiredState =
    isThisOne && ['PREPARING', 'STARTING', 'RUNNING', 'RECONNECTING'].includes(runtime)
      ? 'running'
      : 'stopped'
  return {
    id: String(playlistId),
    user_id: 'local',
    name,
    media_id: '',
    destination_id: LOCAL_DESTINATION.id,
    loop_forever: true,
    desired_state: desired,
    runtime_state: runtime,
    restart_count: isThisOne ? status.supervisor.restart_count : 0,
    last_error: isThisOne ? (status.last_start_error?.message ?? null) : null,
    created_at: '',
    started_at: null,
    stopped_at: null,
    last_heartbeat: null,
    bytes_sent: 0,
    uptime_secs: isThisOne ? status.elapsed_secs : 0,
    ffmpeg_exit_code: null,
  }
}
