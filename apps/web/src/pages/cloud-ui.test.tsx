/**
 * The web screens, driven through a fake transport.
 *
 * Which is also the point of the transport seam: the components can be tested
 * without a server, and the same components would render against the desktop's
 * backend, because neither of them knows the difference.
 */
import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { describe, expect, it, vi } from 'vitest'
import type { ReactElement } from 'react'
import { TransportProvider } from '../TransportContext'
import { CloudDashboard } from './CloudDashboard'
import { DeploymentBanner, ServerStatus } from './ServerStatus'
import { Destinations } from './Destinations'
import { MediaLibrary } from './MediaLibrary'
import type { Broadcast, CloudMedia, Dashboard, StreamDestination } from '../cloud'
import type { Transport } from '../transport'

function broadcast(over: Partial<Broadcast> = {}): Broadcast {
  return {
    id: 'b1',
    user_id: 'u1',
    name: '밤 라디오',
    media_id: 'm1',
    destination_id: 'd1',
    loop_forever: true,
    desired_state: 'stopped',
    runtime_state: 'CREATED',
    restart_count: 0,
    created_at: '',
    bytes_sent: 0,
    uptime_secs: 0,
    ...over,
  }
}

const READY_MEDIA: CloudMedia = {
  id: 'm1',
  user_id: 'u1',
  filename: 'set.mp4',
  size_bytes: 1024 * 1024 * 700,
  state: 'ready',
  duration_secs: 5340,
  width: 1920,
  height: 1080,
  fps: 30,
  video_codec: 'h264',
  audio_codec: 'aac',
  container: 'mp4',
  bitrate_bps: 6_000_000,
  created_at: '',
}

const DESTINATION: StreamDestination = {
  id: 'd1',
  user_id: 'u1',
  label: '내 채널',
  rtmps_url: 'rtmps://a.rtmps.youtube.com/live2',
  key_masked: '••••••••••••',
  created_at: '',
}

/** A transport a test can steer, with every method present. */
function fake(over: Partial<Transport> = {}): Transport {
  const base: Transport = {
    kind: 'web',
    register: vi.fn(),
    login: vi.fn(),
    logout: vi.fn(),
    me: vi.fn(),
    subscription: vi.fn(),
    listMedia: vi.fn().mockResolvedValue([READY_MEDIA]),
    uploadMedia: vi.fn(),
    deleteMedia: vi.fn().mockResolvedValue(undefined),
    listDestinations: vi.fn().mockResolvedValue([DESTINATION]),
    createDestination: vi.fn(),
    deleteDestination: vi.fn().mockResolvedValue(undefined),
    dashboard: vi.fn().mockResolvedValue({
      plan_label: 'Pro',
      active: 0,
      allowed: 2,
      broadcasts: [],
    } satisfies Dashboard),
    createBroadcast: vi.fn(),
    startBroadcast: vi.fn(),
    stopBroadcast: vi.fn(),
    restartBroadcast: vi.fn(),
    deleteBroadcast: vi.fn().mockResolvedValue(undefined),
    logs: vi.fn().mockResolvedValue([]),
    watchDashboard: vi.fn().mockReturnValue(() => undefined),
    health: vi.fn().mockResolvedValue({
      status: 'ok',
      version: '1.0.8',
      deployment: 'cloud',
      checks: { api: true, database: true, ffmpeg: true, storage: true },
    }),
    metrics: vi.fn().mockResolvedValue({
      deployment: 'cloud',
      server: {
        cpu_percent: 12,
        memory_total_bytes: 16_000_000_000,
        memory_available_bytes: 12_000_000_000,
        process_cpu_percent: 1,
        process_memory_bytes: 50_000_000,
        disk_available_bytes: 80_000_000_000,
        egress_bytes: 1_500_000_000,
      },
      broadcasts: [],
    }),
  }
  return { ...base, ...over } as Transport
}

function show(node: ReactElement, transport: Transport) {
  return render(<TransportProvider value={transport}>{node}</TransportProvider>)
}

describe('the broadcast dashboard', () => {
  it("shows the plan's slots as the server reports them", async () => {
    const t = fake({
      dashboard: vi.fn().mockResolvedValue({
        plan_label: 'Business',
        active: 2,
        allowed: 3,
        broadcasts: [
          broadcast({ id: 'b1', desired_state: 'running', runtime_state: 'RUNNING', uptime_secs: 3725 }),
          broadcast({ id: 'b2', name: '낮 라디오', desired_state: 'running', runtime_state: 'RECONNECTING', restart_count: 2 }),
          broadcast({ id: 'b3', name: '쉬는 방송' }),
        ],
      } satisfies Dashboard),
    })

    show(<CloudDashboard />, t)

    expect(await screen.findByTestId('slots')).toHaveTextContent('2 / 3')
    expect(screen.getAllByTestId('broadcast-row')).toHaveLength(3)
    // Reconnecting holds a slot, so it must not look idle.
    expect(screen.getByText('재연결 중')).toBeInTheDocument()
    expect(screen.getByText('재시작 2회')).toBeInTheDocument()
    expect(screen.getByText('송출 1시간 02분 05초')).toBeInTheDocument()
  })

  it('warns when the slots are full, and still lets a running broadcast be stopped', async () => {
    const stop = vi.fn().mockResolvedValue(broadcast({ desired_state: 'stopped' }))
    const t = fake({
      stopBroadcast: stop,
      dashboard: vi.fn().mockResolvedValue({
        plan_label: 'Basic',
        active: 1,
        allowed: 1,
        broadcasts: [broadcast({ desired_state: 'running', runtime_state: 'RUNNING' })],
      } satisfies Dashboard),
    })

    show(<CloudDashboard />, t)

    expect(await screen.findByTestId('slots')).toHaveTextContent('1 / 1')
    expect(screen.getByText(/동시 방송 수를 모두 사용/)).toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: '중지' }))
    expect(stop).toHaveBeenCalledWith('b1')
  })

  it("repeats the server's refusal instead of deciding for itself", async () => {
    // The browser is not the authority here: it asks, and shows the answer.
    const t = fake({
      startBroadcast: vi.fn().mockRejectedValue(new Error('max_concurrent_streams 한도를 초과했습니다 (1/1)')),
      dashboard: vi.fn().mockResolvedValue({
        plan_label: 'Basic',
        active: 0,
        allowed: 1,
        broadcasts: [broadcast()],
      } satisfies Dashboard),
    })

    show(<CloudDashboard />, t)
    await userEvent.click(await screen.findByRole('button', { name: '시작' }))

    expect(await screen.findByRole('alert')).toHaveTextContent('max_concurrent_streams 한도를 초과했습니다 (1/1)')
  })

  it('subscribes once for live updates and unsubscribes on the way out', async () => {
    const stop = vi.fn()
    const watch = vi.fn().mockReturnValue(stop)
    const t = fake({ watchDashboard: watch })

    const view = show(<CloudDashboard />, t)
    await waitFor(() => expect(watch).toHaveBeenCalledTimes(1))
    view.unmount()
    expect(stop).toHaveBeenCalledTimes(1)
  })
})

describe('stream destinations', () => {
  it('sends a key once and leaves no copy anywhere in the page', async () => {
    const KEY = 'abcd-1234-efgh-5678'
    const created = vi.fn().mockResolvedValue(DESTINATION)
    const t = fake({ createDestination: created, listDestinations: vi.fn().mockResolvedValue([]) })

    show(<Destinations />, t)
    await userEvent.click(await screen.findByRole('button', { name: '대상 추가' }))
    await userEvent.type(screen.getByLabelText('스트림 키'), KEY)
    await userEvent.click(screen.getByRole('button', { name: '저장' }))

    await waitFor(() => expect(created).toHaveBeenCalled())
    expect(created.mock.calls[0]?.[0]).toMatchObject({ stream_key: KEY })
    // §9, checked rather than asserted in a comment: not in storage, and not
    // left in the DOM either.
    expect(localStorage.length).toBe(0)
    expect(sessionStorage.length).toBe(0)
    expect(document.body.innerHTML).not.toContain(KEY)
  })

  it('shows dots, never a key, for a saved destination', async () => {
    show(<Destinations />, fake())
    expect(await screen.findByTestId('masked-key')).toHaveTextContent('••••••••••••')
    expect(document.body.innerHTML).not.toContain('abcd')
  })
})

describe('the media library', () => {
  it('shows what is happening to a video without asking the user to decide', async () => {
    const t = fake({
      listMedia: vi.fn().mockResolvedValue([
        { ...READY_MEDIA, id: 'm2', filename: 'new.mp4', state: 'preparing' },
        READY_MEDIA,
      ]),
    })

    show(<MediaLibrary />, t)

    expect(await screen.findByText('변환 중')).toBeInTheDocument()
    expect(screen.getByText('사용 가능')).toBeInTheDocument()
    // No "최적화" anywhere, and no button asking for a decision about it.
    expect(document.body.textContent).not.toContain('최적화')
    // A video still being prepared cannot be deleted out from under the worker.
    const rows = screen.getAllByTestId('media-row')
    expect(rows[0]?.querySelector('button')).toBeDisabled()
  })

  it('reports an upload that the plan refuses', async () => {
    const t = fake({
      uploadMedia: vi.fn().mockRejectedValue(new Error('max_upload_bytes 한도를 초과했습니다 (4096/64)')),
    })

    show(<MediaLibrary />, t)
    await userEvent.upload(
      screen.getByLabelText('영상 파일'),
      new File(['x'], 'big.mp4', { type: 'video/mp4' }),
    )

    expect(await screen.findByRole('alert')).toHaveTextContent('max_upload_bytes 한도를 초과했습니다')
  })
})

describe('where this is running', () => {
  it('says CLOUD when the server says cloud, and says what that means', async () => {
    show(<DeploymentBanner />, fake())

    const banner = await screen.findByTestId('deployment-banner')
    expect(banner).toHaveAttribute('data-deployment', 'cloud')
    expect(banner).toHaveTextContent('REMOTE CLOUD SERVER')
    expect(banner).toHaveTextContent('브라우저나 이 컴퓨터를 꺼도 방송은 계속됩니다')
  })

  it('warns on a laptop, because closing it ends the broadcast', async () => {
    const t = fake({
      health: vi.fn().mockResolvedValue({
        status: 'ok',
        version: '1.0.8',
        deployment: 'local',
        checks: { api: true, database: true, ffmpeg: true, storage: true },
      }),
    })
    show(<DeploymentBanner />, t)

    const banner = await screen.findByTestId('deployment-banner')
    expect(banner).toHaveAttribute('data-deployment', 'local')
    expect(banner).toHaveTextContent('LOCAL DEVELOPMENT')
    expect(banner).toHaveTextContent('컴퓨터를 끄거나 절전되면 방송도 끝납니다')
  })

  it('names the part that is failing rather than just going red', async () => {
    const t = fake({
      health: vi.fn().mockResolvedValue({
        status: 'degraded',
        version: '1.0.8',
        deployment: 'cloud',
        checks: { api: true, database: true, ffmpeg: false, storage: true },
      }),
    })
    show(<DeploymentBanner />, t)

    expect(await screen.findByTestId('health-degraded')).toHaveTextContent('ffmpeg')
  })
})

describe('the metrics a long test needs', () => {
  it('shows per-broadcast uptime, bytes, bitrate, restarts and the pid', async () => {
    const t = fake({
      metrics: vi.fn().mockResolvedValue({
        deployment: 'cloud',
        server: {
          cpu_percent: 7,
          memory_total_bytes: 16_000_000_000,
          memory_available_bytes: 12_000_000_000,
          process_cpu_percent: 1,
          process_memory_bytes: 50_000_000,
          disk_available_bytes: 80_000_000_000,
          egress_bytes: 1_500_000_000,
        },
        broadcasts: [
          {
            id: 'b1',
            name: '밤 라디오',
            runtime_state: 'RUNNING',
            uptime_secs: 3725,
            bytes_sent: 2_793_000_000,
            average_bitrate_bps: 5_998_000,
            restart_count: 2,
            last_error: null,
            ffmpeg_pid: 4242,
            last_heartbeat: null,
          },
        ],
      }),
    })

    show(<ServerStatus />, t)

    const row = await screen.findByTestId('metrics-row')
    expect(row).toHaveTextContent('밤 라디오')
    expect(row).toHaveTextContent('송출 중')
    expect(row).toHaveTextContent('1시간 02분 05초')
    expect(row).toHaveTextContent('6.00 Mbps')
    expect(row).toHaveTextContent('4242')
    // Every check the operator asked for, named rather than rolled into one dot.
    const checks = await screen.findByTestId('health-checks')
    for (const name of ['api', 'database', 'ffmpeg', 'storage']) {
      expect(checks).toHaveTextContent(name)
    }
  })
})
