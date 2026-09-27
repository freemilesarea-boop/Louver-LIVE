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
import { BroadcastForm } from './BroadcastForm'
import { DeploymentBanner, ServerStatus } from './ServerStatus'
import { Destinations } from './Destinations'
import { MediaLibrary } from './MediaLibrary'
import { AUTO_SETTINGS, emptySchedule } from '../cloud'
import type {
  Broadcast, BroadcastDetail, CloudMedia, Dashboard, StreamDestination,
} from '../cloud'
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
    title: '밤 라디오',
    description: '',
    tags: '',
    category: '',
    privacy: 'private',
    settings: AUTO_SETTINGS,
    schedule: emptySchedule(),
    item_count: 1,
    play_count: 1,
    current_index: 0,
    current_position_secs: 0,
    current_duration_secs: 0,
    cycle_duration_secs: 0,
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
  kind: 'manual_rtmps',
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
    getBroadcast: vi.fn(),
    updateBroadcast: vi.fn(),
    replaceItems: vi.fn().mockResolvedValue([]),
    logs: vi.fn().mockResolvedValue([]),
    watchDashboard: vi.fn().mockReturnValue(() => undefined),
    health: vi.fn().mockResolvedValue({
      status: 'ok',
      version: '1.0.8',
      deployment: 'cloud',
      checks: { api: true, database: true, ffmpeg: true, ffmpeg_rtmps: true, storage: true },
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
          broadcast({
            id: 'b1',
            desired_state: 'running',
            runtime_state: 'RUNNING',
            uptime_secs: 3725,
            current_index: 2,
            play_count: 8,
            current_item: 'Jazz Night 02.mp4',
            next_item: 'Jazz Night 03.mp4',
            current_position_secs: 1934,
            current_duration_secs: 5400,
          }),
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
    expect(screen.getByText('1시간 02분 05초')).toBeInTheDocument()
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
        checks: { api: true, database: true, ffmpeg: true, ffmpeg_rtmps: true, storage: true },
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
        checks: { api: true, database: true, ffmpeg: false, ffmpeg_rtmps: false, storage: true },
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

describe('the operations card', () => {
  it('shows what is playing, what is next, and where in the playlist', async () => {
    const t = fake({
      dashboard: vi.fn().mockResolvedValue({
        plan_label: 'Business',
        active: 1,
        allowed: 3,
        broadcasts: [
          broadcast({
            desired_state: 'running',
            runtime_state: 'RUNNING',
            uptime_secs: 7200,
            bytes_sent: 5_400_000_000,
            item_count: 8,
            play_count: 8,
            current_index: 2,
            current_item: 'Jazz Night 02.mp4',
            next_item: 'Jazz Night 03.mp4',
            current_position_secs: 1934,
            current_duration_secs: 5400,
          }),
        ],
      } satisfies Dashboard),
    })

    show(<CloudDashboard />, t)

    expect(await screen.findByTestId('now-playing')).toHaveTextContent('Jazz Night 02.mp4')
    expect(screen.getByTestId('next-up')).toHaveTextContent('Jazz Night 03.mp4')
    expect(screen.getByTestId('playlist-progress')).toHaveTextContent('2 / 8')
    // 32:14 of 1:30:00, as the brief's example asks for.
    expect(screen.getByText('32분 14초 / 1시간 30분 00초')).toBeInTheDocument()
    // 5.4 GB over two hours is 6 Mbps.
    expect(screen.getByText('6.00 Mbps')).toBeInTheDocument()
    // "송출 중" means FFmpeg is publishing to an ingest — not that YouTube has
    // made it public. The card says which, and where it is sending.
    expect(screen.getByText('RTMPS 전송 중')).toBeInTheDocument()
    expect(screen.getByText(/a\.rtmps\.youtube\.com/)).toBeInTheDocument()
    expect(screen.getByText(/실시간 시작/)).toBeInTheDocument()
    // A live broadcast offers a stop, never a delete.
    expect(screen.getByRole('button', { name: '중지' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '삭제' })).not.toBeInTheDocument()
  })

  it('says a stopped broadcast is scheduled, and when', async () => {
    const tomorrow = new Date(Date.now() + 86_400_000)
    tomorrow.setHours(21, 0, 0, 0)
    const t = fake({
      dashboard: vi.fn().mockResolvedValue({
        plan_label: 'Pro',
        active: 0,
        allowed: 2,
        broadcasts: [
          broadcast({
            schedule: { ...emptySchedule(), enabled: true, start_at: tomorrow.toISOString(), repeat_days: 0b111_1111 },
          }),
        ],
      } satisfies Dashboard),
    })

    show(<CloudDashboard />, t)
    expect(await screen.findByText('예약됨')).toBeInTheDocument()
    expect(screen.getByTestId('schedule-label')).toHaveTextContent('매일')
  })
})

describe('making a broadcast', () => {
  const VIDEOS: CloudMedia[] = [
    { ...READY_MEDIA, id: 'm1', filename: 'one.mp4', duration_secs: 60 },
    { ...READY_MEDIA, id: 'm2', filename: 'two.mp4', duration_secs: 120 },
    { ...READY_MEDIA, id: 'm3', filename: 'three.mp4', duration_secs: 30 },
  ]

  function withVideos(over: Partial<Transport> = {}) {
    return fake({ listMedia: vi.fn().mockResolvedValue(VIDEOS), ...over })
  }

  it('builds a playlist of several videos and sends it in order', async () => {
    const created = vi.fn().mockResolvedValue({})
    const t = withVideos({ createBroadcast: created })
    show(<BroadcastForm onDone={() => undefined} onCancel={() => undefined} />, t)

    for (const name of ['+ one.mp4', '+ two.mp4', '+ three.mp4']) {
      await userEvent.click(await screen.findByRole('button', { name }))
    }
    expect(screen.getAllByTestId('playlist-row')).toHaveLength(3)

    await userEvent.type(screen.getByLabelText('이름'), '밤 라디오')
    await userEvent.click(screen.getByRole('button', { name: /^방송 만들기$/ }))

    await waitFor(() => expect(created).toHaveBeenCalled())
    const sent = created.mock.calls[0]?.[0]
    expect(sent.items.map((i: { media_id: string }) => i.media_id)).toEqual(['m1', 'm2', 'm3'])
    expect(sent.name).toBe('밤 라디오')
    expect(sent.loop_forever).toBe(true)
    expect(sent.destination_id).toBe('d1')
  })

  it('reorders, repeats and disables, and sends exactly that', async () => {
    const created = vi.fn().mockResolvedValue({})
    const t = withVideos({ createBroadcast: created })
    show(<BroadcastForm onDone={() => undefined} onCancel={() => undefined} />, t)

    await userEvent.click(await screen.findByRole('button', { name: '+ one.mp4' }))
    await userEvent.click(screen.getByRole('button', { name: '+ two.mp4' }))
    await userEvent.click(screen.getByRole('button', { name: '+ three.mp4' }))

    // Move the third to the top, repeat it twice, and switch the second off.
    await userEvent.click(screen.getByRole('button', { name: 'three.mp4 위로' }))
    await userEvent.click(screen.getByRole('button', { name: 'three.mp4 위로' }))
    await userEvent.clear(screen.getByLabelText('three.mp4 반복 횟수'))
    await userEvent.type(screen.getByLabelText('three.mp4 반복 횟수'), '2')
    await userEvent.click(screen.getByLabelText('two.mp4 사용'))

    await userEvent.click(screen.getByRole('button', { name: /^방송 만들기$/ }))
    await waitFor(() => expect(created).toHaveBeenCalled())

    expect(created.mock.calls[0]?.[0].items).toEqual([
      { media_id: 'm3', enabled: true, repeat_count: 2 },
      { media_id: 'm1', enabled: true, repeat_count: 1 },
      { media_id: 'm2', enabled: false, repeat_count: 1 },
    ])
  })

  it('cannot be saved with an empty playlist', async () => {
    const t = withVideos()
    show(<BroadcastForm onDone={() => undefined} onCancel={() => undefined} />, t)
    expect(await screen.findByRole('button', { name: /^방송 만들기$/ })).toBeDisabled()
    await userEvent.click(screen.getByRole('button', { name: '+ one.mp4' }))
    expect(screen.getByRole('button', { name: /^방송 만들기$/ })).toBeEnabled()
  })

  it('keeps 247streams metadata separate from what YouTube shows', async () => {
    const t = withVideos()
    show(<BroadcastForm onDone={() => undefined} onCancel={() => undefined} />, t)
    // The warning is the point: a pasted stream key cannot set a YouTube title,
    // and the form says so where the title is typed.
    expect(await screen.findByText(/YouTube의 제목·설명·공개/)).toBeInTheDocument()
  })

  it('hides the sending settings until asked, and defaults to auto', async () => {
    const created = vi.fn().mockResolvedValue({})
    const t = withVideos({ createBroadcast: created })
    show(<BroadcastForm onDone={() => undefined} onCancel={() => undefined} />, t)

    expect(await screen.findByText(/자동 — 서버가 영상에 맞는/)).toBeInTheDocument()
    expect(screen.queryByLabelText('해상도')).not.toBeInTheDocument()

    await userEvent.click(screen.getByRole('button', { name: '고급 설정' }))
    expect(screen.getByLabelText('해상도')).toHaveValue('auto')

    await userEvent.click(screen.getByRole('button', { name: '+ one.mp4' }))
    await userEvent.click(screen.getByRole('button', { name: /^방송 만들기$/ }))
    await waitFor(() => expect(created).toHaveBeenCalled())
    expect(created.mock.calls[0]?.[0].settings).toEqual({
      resolution: 'auto',
      fps: 'auto',
      video_bitrate_kbps: 0,
      audio_bitrate_kbps: 0,
    })
  })

  it('schedules in the reader own clock and sends UTC', async () => {
    const created = vi.fn().mockResolvedValue({})
    const t = withVideos({ createBroadcast: created })
    show(<BroadcastForm onDone={() => undefined} onCancel={() => undefined} />, t)

    await userEvent.click(await screen.findByRole('button', { name: '+ one.mp4' }))
    await userEvent.click(screen.getByRole('switch', { name: '예약 시작' }))
    const box = screen.getByLabelText('시작 시각')
    await userEvent.type(box, '2026-11-01T21:00')
    await userEvent.click(screen.getByRole('button', { name: '매일' }))
    await userEvent.click(screen.getByRole('button', { name: /^방송 만들기$/ }))

    await waitFor(() => expect(created).toHaveBeenCalled())
    const sched = created.mock.calls[0]?.[0].schedule
    expect(sched.enabled).toBe(true)
    expect(sched.repeat_days).toBe(0b111_1111)
    // Stored as an instant, not as the text that was typed.
    expect(new Date(sched.start_at).toISOString()).toBe(sched.start_at)
    expect(new Date(sched.start_at).getHours()).toBe(21)
  })

  it('saves an edit as a patch and a playlist replacement', async () => {
    const detail: BroadcastDetail = {
      ...broadcast({ id: 'b9', name: '기존', item_count: 1 }),
      items: [
        {
          id: 'i1',
          broadcast_id: 'b9',
          media_id: 'm1',
          position: 0,
          enabled: true,
          repeat_count: 1,
          filename: 'one.mp4',
          duration_secs: 60,
          state: 'ready',
        },
      ],
    }
    const update = vi.fn().mockResolvedValue(detail)
    const replace = vi.fn().mockResolvedValue([])
    const t = withVideos({ updateBroadcast: update, replaceItems: replace })

    show(<BroadcastForm editing={detail} onDone={() => undefined} onCancel={() => undefined} />, t)

    // The existing playlist is there to edit, not a blank one.
    expect(await screen.findByText('one.mp4')).toBeInTheDocument()
    await userEvent.click(screen.getByRole('button', { name: '+ two.mp4' }))
    await userEvent.click(screen.getByRole('button', { name: '저장' }))

    await waitFor(() => expect(replace).toHaveBeenCalled())
    expect(update.mock.calls[0]?.[0]).toBe('b9')
    expect(replace.mock.calls[0]?.[1]).toEqual([
      { media_id: 'm1', enabled: true, repeat_count: 1 },
      { media_id: 'm2', enabled: true, repeat_count: 1 },
    ])
  })
})
