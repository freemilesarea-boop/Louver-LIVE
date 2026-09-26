/**
 * The cloud domain, as the UI sees it.
 *
 * These mirror `louver-cloud`'s serialised models. A stream key is absent by
 * construction: the server sends `key_masked` and there is no field here for a
 * key, so no component can display one and no store can persist one.
 */

export type DesiredState = 'stopped' | 'running'

/** §2's lifecycle, in the server's own words. */
export type RuntimeState =
  | 'CREATED' | 'PREPARING' | 'STARTING' | 'RUNNING'
  | 'RECONNECTING' | 'STOPPING' | 'STOPPED' | 'FAILED'

export type MediaState = 'uploaded' | 'analysing' | 'preparing' | 'ready' | 'failed'

export interface Me {
  id: string
  email: string
  plan_id: string
}

export interface Subscription {
  user_id: string
  plan_id: string
  plan_label: string
  status: string
  limits: Record<string, number>
}

export interface CloudMedia {
  id: string
  user_id: string
  filename: string
  size_bytes: number
  state: MediaState
  duration_secs: number
  width: number
  height: number
  fps: number
  video_codec: string
  audio_codec?: string | null
  container: string
  bitrate_bps: number
  prepared_duration_secs?: number | null
  last_error?: string | null
  created_at: string
}

export interface StreamDestination {
  id: string
  user_id: string
  label: string
  rtmps_url: string
  /** Always dots. The key itself never leaves the server. */
  key_masked: string
  created_at: string
}

export interface Broadcast {
  id: string
  user_id: string
  name: string
  media_id: string
  destination_id: string
  loop_forever: boolean
  desired_state: DesiredState
  runtime_state: RuntimeState
  restart_count: number
  last_error?: string | null
  created_at: string
  started_at?: string | null
  stopped_at?: string | null
  last_heartbeat?: string | null
  bytes_sent: number
  uptime_secs: number
  ffmpeg_exit_code?: number | null
}

export interface Dashboard {
  plan_label: string
  /** How many of this account's broadcasts hold a slot right now. */
  active: number
  /** How many the plan allows. The server decides this; the UI only shows it. */
  allowed: number
  broadcasts: Broadcast[]
}

export interface BroadcastEvent {
  id: number
  broadcast_id: string
  at: string
  level: string
  message: string
}

export interface NewDestination {
  label: string
  rtmps_url: string
  /** Passed straight through to the server and never kept afterwards. */
  stream_key: string
}

export interface NewBroadcast {
  name: string
  media_id: string
  destination_id: string
  loop_forever?: boolean
}

/** Korean labels for §2's states, so no component spells them itself. */
export const RUNTIME_LABELS: Record<RuntimeState, string> = {
  CREATED: '대기',
  PREPARING: '준비 중',
  STARTING: '시작 중',
  RUNNING: '송출 중',
  RECONNECTING: '재연결 중',
  STOPPING: '중지 중',
  STOPPED: '중지됨',
  FAILED: '실패',
}

export const MEDIA_LABELS: Record<MediaState, string> = {
  uploaded: '업로드됨',
  analysing: '분석 중',
  preparing: '변환 중',
  ready: '사용 가능',
  failed: '실패',
}

/** True when this broadcast is holding one of the plan's slots. */
export function holdsASlot(b: Broadcast): boolean {
  return ['PREPARING', 'STARTING', 'RUNNING', 'RECONNECTING'].includes(b.runtime_state)
}

/** What `/health` answers. No session needed: a monitor is usually asking. */
export interface Health {
  status: 'ok' | 'degraded'
  version: string
  /** `local` — this computer. `cloud` — a server that stays on. */
  deployment: 'local' | 'cloud'
  checks: {
    api: boolean
    database: boolean
    ffmpeg: boolean
    storage: boolean
  }
}

export interface BroadcastMetrics {
  id: string
  name: string
  runtime_state: RuntimeState
  uptime_secs: number
  bytes_sent: number
  average_bitrate_bps: number
  restart_count: number
  last_error?: string | null
  ffmpeg_pid?: number | null
  last_heartbeat?: string | null
}

export interface Metrics {
  deployment: 'local' | 'cloud'
  server: {
    cpu_percent: number
    memory_total_bytes: number
    memory_available_bytes: number
    process_cpu_percent: number
    process_memory_bytes: number
    disk_available_bytes: number
    egress_bytes: number
  }
  broadcasts: BroadcastMetrics[]
}

/** What the header says about where this is running, and why it matters. */
export const DEPLOYMENT_LABELS: Record<Health['deployment'], { title: string; hint: string }> = {
  local: {
    title: 'LOCAL DEVELOPMENT',
    hint: '이 컴퓨터에서 서버가 실행 중입니다. 컴퓨터를 끄거나 절전되면 방송도 끝납니다.',
  },
  cloud: {
    title: 'REMOTE CLOUD SERVER',
    hint: '서버에서 방송이 실행됩니다. 브라우저나 이 컴퓨터를 꺼도 방송은 계속됩니다.',
  },
}
