/**
 * UI end-to-end journey (§58).
 *
 * Drives the real React app through the scenario the spec lists: launch, add
 * videos, check the playlist, optimize, reorder, set a schedule, run a dry
 * run, see the live state, stop, change settings, restart and find them
 * restored.
 */
import { describe, expect, it, beforeEach } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { App } from '@/App'
import { setMockBackend } from '@/services/ipc'
import { createMockBackend } from '@/test/mockBackend'
import { useAppStore } from '@/stores/useAppStore'

type Backend = ReturnType<typeof createMockBackend>
let backend: Backend

function mount(opts: Parameters<typeof createMockBackend>[0] = {}) {
  backend = createMockBackend({ seedSettings: { first_run_complete: 'true' }, ...opts })
  setMockBackend(backend)
  return render(<App />)
}

function resetStore() {
  useAppStore.setState({
    page: 'dashboard', status: null, metrics: null, settings: null,
    playlists: [], activePlaylistId: null, activePlaylist: null,
    media: [], schedules: [], normalizing: null, toasts: [],
    startupNotice: null, booted: false,
  })
}

beforeEach(() => {
  resetStore()
})

async function gotoPage(user: ReturnType<typeof userEvent.setup>, label: string) {
  await user.click(screen.getByRole('button', { name: label }))
}

/** Create a playlist and add three videos to it. */
async function buildPlaylist(user: ReturnType<typeof userEvent.setup>, name = 'Night Jazz') {
  await gotoPage(user, '플레이리스트')
  await user.click(await screen.findByRole('button', { name: '플레이리스트 만들기' }))
  await user.type(screen.getByLabelText('플레이리스트 이름'), name)
  await user.click(screen.getByRole('button', { name: '만들기' }))
  await user.click(await screen.findByRole('button', { name: '+ 영상 추가' }))
  await screen.findByText('night01.mp4')
}

describe('first run', () => {
  it('shows the wizard on a fresh install and not afterwards', async () => {
    const user = userEvent.setup()
    backend = createMockBackend({ seedSettings: {} })
    setMockBackend(backend)
    render(<App />)

    expect(await screen.findByText('LOUVER LIVE')).toBeInTheDocument()
    expect(screen.getByText(/자동으로 순차 반복하며/)).toBeInTheDocument()

    // Step through and finish.
    await user.click(screen.getByRole('button', { name: /다음/ }))
    await user.type(screen.getByLabelText('스트림 키'), 'wxyz-wxyz-wxyz-wxyz')
    await user.click(screen.getByRole('button', { name: /다음/ }))
    await user.click(screen.getByRole('button', { name: /다음/ }))
    await user.click(screen.getByRole('button', { name: /다음/ }))
    await user.click(screen.getByRole('button', { name: '시작하기' }))

    // The main shell replaces the wizard.
    expect(await screen.findByRole('button', { name: '대시보드' })).toBeInTheDocument()
    expect(backend.snapshotSettings().first_run_complete).toBe('true')
  })
})

describe('the main journey', () => {
  it('adds videos, optimizes them, and reports the playlist total', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })

    await buildPlaylist(user)

    // All three landed, in order.
    const list = screen.getByRole('list', { name: '영상 목록' })
    const names = within(list).getAllByText(/night0\d\.mp4/).map((n) => n.textContent)
    expect(names).toEqual(['night01.mp4', 'night02.mp4', 'night03.mp4'])

    // They are not broadcastable yet, and the UI says so (§7).
    expect(screen.getByText(/3개 영상이 방송 규격과 다릅니다/)).toBeInTheDocument()

    // The disk plan is shown before any encoding starts (§10).
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    expect(await screen.findByText('저장 공간 확인')).toBeInTheDocument()
    expect(screen.getByText(/예상 추가 공간/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: '최적화 시작' }))

    await waitFor(() => {
      expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument()
    })
    expect(await screen.findAllByText('송출 준비 완료')).toHaveLength(3)
    // Three ~1h clips, so the total is reported in hours (§25).
    expect(screen.getByTestId('playlist-total')).toHaveTextContent(/^\d+시간 \d\d분 \d\d초$/)
  })

  it('explains why a file needs optimizing instead of just failing', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)

    await user.click(screen.getAllByRole('button', { name: '최적화 필요' })[0]!)
    expect(await screen.findByText(/해상도가 1920x1080이 아닙니다/)).toBeInTheDocument()
  })

  it('reorders the playlist by drag and drop and persists the new order', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)

    const list = screen.getByRole('list', { name: '영상 목록' })
    const rows = within(list).getAllByRole('listitem')
    // Drag the third item onto the first. Each event gets its own act() so the
    // drag source recorded by dragstart has committed before drop reads it.
    await act(async () => { fireEvent.dragStart(rows[2]!) })
    await act(async () => { fireEvent.dragOver(rows[0]!) })
    await act(async () => { fireEvent.drop(rows[0]!) })

    await waitFor(() => {
      const names = within(screen.getByRole('list', { name: '영상 목록' }))
        .getAllByText(/night0\d\.mp4/)
        .map((n) => n.textContent)
      expect(names[0]).toBe('night03.mp4')
    })
  })

  it('blocks a broadcast whose playlist is not optimized, naming the reason', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await gotoPage(user, '대시보드')

    await user.click(await screen.findByRole('button', { name: /방송 시작/ }))
    expect(await screen.findByText('방송 시작 전 점검')).toBeInTheDocument()
    expect(screen.getByText(/최적화가 필요합니다/)).toBeInTheDocument()
    expect(screen.getByText('LL-STREAM-003')).toBeInTheDocument()
  })

  it('runs a dry run without a stream key and shows TEST rather than LIVE', async () => {
    const user = userEvent.setup()
    mount({ hasStreamKey: false })
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())

    await gotoPage(user, '대시보드')
    await user.click(await screen.findByRole('button', { name: /로컬 테스트/ }))

    // §30: it goes live locally, and §61 says it must not read as on-air.
    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })
    expect(screen.getAllByText('TEST').length).toBeGreaterThan(0)
    expect(screen.getByText(/로컬 테스트 모드/)).toBeInTheDocument()
  })

  it('warns about uptime before the first real broadcast, then goes live and stops', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())
    await gotoPage(user, '대시보드')

    await user.click(await screen.findByRole('button', { name: /방송 시작/ }))

    // §62's three warnings, shown once.
    expect(await screen.findByText('방송을 시작하기 전에')).toBeInTheDocument()
    expect(screen.getByText(/컴퓨터와 인터넷 연결이 유지되어야/)).toBeInTheDocument()
    expect(screen.getByText(/덮개를 닫으면/)).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: '확인하고 시작' }))

    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })
    expect(screen.getAllByText('LIVE').length).toBeGreaterThan(0)
    // Twice on purpose: the readiness strip states the configured mode, and
    // the broadcast card badges the mode the live session is running in.
    expect(screen.getAllByText('STREAM COPY')).toHaveLength(2)

    // §26: stopping requires a confirmation.
    await user.click(screen.getByRole('button', { name: /방송 종료/ }))
    expect(await screen.findByText('방송을 종료할까요?')).toBeInTheDocument()
    await user.click(within(screen.getByRole('dialog')).getByRole('button', { name: '방송 종료' }))

    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'STOPPED')
    })
    expect(screen.getAllByRole('button', { name: /방송 시작/ }).length).toBeGreaterThan(0)
  })
})

describe('scheduling', () => {
  it('creates an overnight schedule and shows that it crosses midnight', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)

    await gotoPage(user, '방송 예약')
    await user.clear(screen.getByLabelText('시작 시간'))
    await user.type(screen.getByLabelText('시작 시간'), '20:00')
    await user.clear(screen.getByLabelText('종료 시간'))
    await user.type(screen.getByLabelText('종료 시간'), '08:00')

    // §19: the UI must make the midnight crossing obvious.
    expect(await screen.findByText(/20:00 → 다음 날 08:00/)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: '매일' }))
    await user.click(screen.getByRole('button', { name: /예약 추가/ }))

    const list = await screen.findByText('20:00 → 08:00')
    expect(list).toBeInTheDocument()
    expect(screen.getByText('자정 넘김')).toBeInTheDocument()
    expect(screen.getAllByText('매일').length).toBeGreaterThan(0)
  })

  it('refuses a schedule with no days selected', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await gotoPage(user, '방송 예약')

    // Clear every day.
    for (const d of ['월', '화', '수', '목', '금', '토', '일']) {
      const btn = screen.getAllByRole('button', { name: d }).find((b) => b.getAttribute('aria-pressed') === 'true')
      if (btn) await user.click(btn)
    }
    await user.click(screen.getByRole('button', { name: /예약 추가/ }))
    expect(await screen.findByText(/요일을 하나 이상/)).toBeInTheDocument()
  })

  it('deletes a schedule', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await gotoPage(user, '방송 예약')
    await user.click(screen.getByRole('button', { name: /예약 추가/ }))
    await screen.findByText(/20:00 → 08:00/)

    await user.click(screen.getByRole('button', { name: '예약 삭제' }))
    await waitFor(() => {
      expect(screen.getByText('예약된 방송이 없습니다')).toBeInTheDocument()
    })
  })
})

describe('settings', () => {
  it('saves a stream key, shows it masked, and reveals it only after confirmation', async () => {
    const user = userEvent.setup()
    mount({ hasStreamKey: false })
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')

    await user.type(await screen.findByLabelText('YouTube 스트림 키'), 'abcd-efgh-ijkl-mnop')
    await user.click(screen.getByRole('button', { name: '저장' }))

    // §15: only the tail is ever shown by default.
    expect(await screen.findByText('저장됨 · ••••mnop')).toBeInTheDocument()
    expect(screen.queryByText('abcd-efgh-ijkl-mnop')).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: '스트림 키 보기' }))
    expect(await screen.findByText(/화면을 볼 수 있는 사람이 없는지/)).toBeInTheDocument()
    expect(screen.queryByText('abcd-efgh-ijkl-mnop')).not.toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: '표시' }))
    expect(await screen.findByText('abcd-efgh-ijkl-mnop')).toBeInTheDocument()
  })

  it('rejects a stream key containing a path separator', async () => {
    const user = userEvent.setup()
    mount({ hasStreamKey: false })
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')
    await user.type(await screen.findByLabelText('YouTube 스트림 키'), 'abc/def')
    await user.click(screen.getByRole('button', { name: '저장' }))
    expect(await screen.findByText('LL-CONFIG-001')).toBeInTheDocument()
  })

  it('persists settings across a restart', async () => {
    const user = userEvent.setup()
    const { unmount } = mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')

    await user.click(await screen.findByRole('switch', { name: /자동 실행/ }))
    await user.selectOptions(screen.getByLabelText('송출 품질'), '720p30')
    await waitFor(() => {
      expect(backend.snapshotSettings().launch_at_startup).toBe('true')
      expect(backend.snapshotSettings().output_profile).toBe('720p30')
    })

    // Restart the app with the settings the previous run saved.
    const saved = backend.snapshotSettings()
    unmount()
    resetStore()
    backend = createMockBackend({ seedSettings: saved })
    setMockBackend(backend)
    render(<App />)

    await user.click(await screen.findByRole('button', { name: '설정' }))
    expect(await screen.findByRole('switch', { name: /자동 실행/ })).toBeChecked()
    expect(screen.getByLabelText('송출 품질')).toHaveValue('720p30')
  })

  it('warns that clearing the cache invalidates files the playlist uses', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())

    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: '캐시 전체 삭제' }))
    expect(await screen.findByText(/사용 중인 최적화 파일 3개가 함께 삭제됩니다/)).toBeInTheDocument()
  })
})

describe('error presentation', () => {
  it('shows a Korean message with the technical detail behind a disclosure', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })

    useAppStore.getState().reportError({
      code_str: 'LL-NETWORK-002',
      message: '유튜브 서버에 연결하지 못했습니다. 스트림 키와 인터넷 연결을 확인해주세요.',
      detail: 'rtmps://a.rtmps.youtube.com/live2/•••••••• Connection refused',
    })

    const alert = await screen.findByRole('alert')
    expect(within(alert).getByText(/유튜브 서버에 연결하지 못했습니다/)).toBeInTheDocument()
    expect(within(alert).getByText('LL-NETWORK-002')).toBeInTheDocument()
    // §35: the raw FFmpeg text is not the headline.
    expect(within(alert).queryByText(/Connection refused/)).not.toBeInTheDocument()

    await user.click(within(alert).getByRole('button', { name: '상세정보' }))
    expect(within(alert).getByText(/Connection refused/)).toBeInTheDocument()
    // And even the detail carries no key.
    expect(within(alert).queryByText(/abcd-efgh/)).not.toBeInTheDocument()
  })
})

describe('logs', () => {
  it('lists recent events after a broadcast', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())
    await gotoPage(user, '대시보드')
    await user.click(await screen.findByRole('button', { name: /로컬 테스트/ }))
    await waitFor(() => expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE'))

    await gotoPage(user, '로그')
    const events = await screen.findByTestId('recent-events')
    expect(within(events).getByText(/방송 시작: Night Jazz/)).toBeInTheDocument()
    expect(screen.getByText(/스트림 키는 로그에 기록되지 않습니다/)).toBeInTheDocument()
  })
})

describe('developer diagnostics', () => {
  it('shows what the live FFmpeg command is actually doing', async () => {
    const user = userEvent.setup()
    mount({ seedSettings: { first_run_complete: 'true', developer_mode: 'true' } })
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())
    await gotoPage(user, '대시보드')
    await user.click(await screen.findByRole('button', { name: /로컬 테스트/ }))
    await waitFor(() => expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE'))

    await gotoPage(user, '설정')
    // §13: the panel reports the running command, not just the setting.
    //
    // It polls on a three-second interval, so the default one-second wait is
    // too tight on a loaded machine — this failed only when the Rust build was
    // running alongside it.
    const polled = { timeout: 5000 }
    expect(await screen.findByText('Streaming Mode', {}, polled)).toBeInTheDocument()
    expect(await screen.findByText(/영상 재인코딩 없음/, {}, polled)).toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: '실행 중인 명령 보기' }))
    const cmd = await screen.findByText(/-f concat/)
    expect(cmd).toHaveTextContent('-c copy')
    // And the displayed command carries no stream key.
    expect(cmd.textContent).not.toMatch(/[a-z0-9]{4}(-[a-z0-9]{4}){3}/i)
    expect(cmd).toHaveTextContent('••••••••')
  })

  it('explains a high-CPU session as compatibility mode rather than leaving it a mystery', async () => {
    const user = userEvent.setup()
    mount({
      seedSettings: {
        first_run_complete: 'true',
        developer_mode: 'true',
        stream_mode: 'compatibility_encode',
      },
    })
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())
    await gotoPage(user, '대시보드')
    await user.click(await screen.findByRole('button', { name: /로컬 테스트/ }))
    await waitFor(() => expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE'))

    await gotoPage(user, '설정')
    expect(await screen.findByText(/실시간 재인코딩 중/)).toBeInTheDocument()
    expect(screen.getByText(/영상 인코더 인자/)).toBeInTheDocument()
  })
})

describe('broadcast settings and the chat bot', () => {
  it('builds metadata with tag chips and refuses a tag list over the limit', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '방송 설정')

    await user.type(
      await screen.findByLabelText('방송 제목'),
      'PLAYLIST for your room | lofi, chill, jazz mood',
    )
    await user.type(screen.getByLabelText('방송 설명'), 'lofi all night')

    // Enter and comma both commit a tag, and duplicates are dropped.
    const tagInput = screen.getByLabelText('태그 입력')
    await user.type(tagInput, 'lofi{Enter}')
    await user.type(tagInput, 'jazz,')
    await user.type(tagInput, 'LOFI{Enter}')
    expect(screen.getByText('lofi')).toBeInTheDocument()
    expect(screen.getByText('jazz')).toBeInTheDocument()
    expect(screen.queryByText('LOFI')).not.toBeInTheDocument()

    // A tag can be taken back off.
    await user.click(screen.getByLabelText('jazz 태그 삭제'))
    expect(screen.queryByText('jazz')).not.toBeInTheDocument()

    await user.click(screen.getByRole('button', { name: '저장' }))
    expect(await screen.findByText(/방송 정보를 저장했습니다/)).toBeInTheDocument()
  })

  it('counts a spaced tag against the budget the way YouTube does', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '방송 설정')

    const tagInput = await screen.findByLabelText('태그 입력')
    await user.type(tagInput, 'work music{Enter}')
    // 10 characters plus the two quotes YouTube adds.
    expect(screen.getByText(/12 \/ 500자/)).toBeInTheDocument()
  })

  it('saves a preset and puts it back when chosen', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '방송 설정')

    await user.type(await screen.findByLabelText('방송 제목'), 'ROOM. 24/7 lofi')
    await user.type(screen.getByLabelText('프리셋 이름'), 'ROOM. 24/7')
    await user.click(screen.getByRole('button', { name: /현재 내용을 프리셋으로 저장/ }))
    expect(await screen.findByRole('button', { name: 'ROOM. 24/7' })).toBeInTheDocument()

    // Change the title, then load the preset back over it.
    await user.clear(screen.getByLabelText('방송 제목'))
    await user.type(screen.getByLabelText('방송 제목'), 'something else')
    await user.click(screen.getByRole('button', { name: 'ROOM. 24/7' }))
    await waitFor(() => {
      expect(screen.getByLabelText('방송 제목')).toHaveValue('ROOM. 24/7 lofi')
    })
  })

  it('manages the chat rotation and will not accept an interval under five minutes', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '방송 설정')

    const input = await screen.findByLabelText('새 메시지')
    await user.type(input, '🎧 구독해주세요{Enter}')
    await user.type(input, '🌙 편안한 시간 보내세요{Enter}')
    expect(await screen.findByLabelText('메시지 1')).toHaveValue('🎧 구독해주세요')
    expect(screen.getByLabelText('메시지 2')).toHaveValue('🌙 편안한 시간 보내세요')

    // Reordering swaps them.
    await user.click(screen.getByLabelText('메시지 2 위로'))
    await waitFor(() => {
      expect(screen.getByLabelText('메시지 1')).toHaveValue('🌙 편안한 시간 보내세요')
    })

    // The interval control offers nothing below the floor (§6).
    const interval = screen.getByLabelText('전송 간격') as HTMLSelectElement
    const offered = Array.from(interval.options).map((o) => Number(o.value))
    expect(Math.min(...offered)).toBeGreaterThanOrEqual(300)

    await user.click(screen.getByLabelText('메시지 2 삭제'))
    await waitFor(() => expect(screen.queryByLabelText('메시지 2')).not.toBeInTheDocument())
  })

  it('offers automatic apply on start, and remembers the choice', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '방송 설정')

    // The page claims metadata is applied when a broadcast starts, so the
    // switch that controls it has to be visible and real.
    const toggle = await screen.findByLabelText('방송을 시작할 때 자동으로 적용')
    expect(toggle).toBeChecked()
    await user.click(toggle)
    await waitFor(() => expect(toggle).not.toBeChecked())

    await gotoPage(user, '대시보드')
    await gotoPage(user, '방송 설정')
    await waitFor(() => {
      expect(screen.getByLabelText('방송을 시작할 때 자동으로 적용')).not.toBeChecked()
    })
  })

  it('connects a YouTube account and shows the channel rather than the token', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')

    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    // Consent happens in a browser, so the page polls for the result rather
    // than blocking; give it more than one poll interval.
    const polled = { timeout: 6000 }
    expect(await screen.findByText('ROOM.', {}, polled)).toBeInTheDocument()
    expect(screen.getByText('UC-room')).toBeInTheDocument()
    expect(screen.getByText('macOS 키체인')).toBeInTheDocument()
    // Nothing token-shaped is rendered anywhere on the page.
    expect(document.body.textContent).not.toMatch(/1\/\/|ya29\./)

    await user.click(screen.getByRole('button', { name: '연결 해제' }))
    expect(await screen.findByRole('button', { name: 'YouTube 계정 연결' }, polled)).toBeInTheDocument()
  })
})
