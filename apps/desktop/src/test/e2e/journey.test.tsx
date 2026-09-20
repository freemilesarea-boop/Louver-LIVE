/**
 * UI end-to-end journey (§58).
 *
 * Drives the real React app through the scenario the spec lists: launch, add
 * videos, check the playlist, optimize, reorder, set a schedule, run a dry
 * run, see the live state, stop, change settings, restart and find them
 * restored.
 */
import { describe, expect, it, afterEach, beforeEach } from 'vitest'
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { App } from '@/App'
import { setMockBackend } from '@/services/ipc'
import { createMockBackend } from '@/test/mockBackend'
import { useAppStore } from '@/stores/useAppStore'

type Backend = ReturnType<typeof createMockBackend>
let backend: Backend

function mount(opts: Parameters<typeof createMockBackend>[0] = {}) {
  backend?.stop()
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

// The mock keeps a one-second interval running for as long as a broadcast is
// "live". Left behind, those pile up across the suite and slow later tests
// enough to time out queries that are correct.
afterEach(() => {
  backend?.stop()
})

async function gotoPage(user: ReturnType<typeof userEvent.setup>, label: string) {
  await user.click(screen.getByRole('button', { name: label }))
}

/**
 * Press 방송 시작 for a test that is not about the uptime warning.
 *
 * The warning is shown once per install and only once the settings have
 * loaded, so waiting on it here is a race. It has a test of its own; every
 * other test marks it seen first and goes straight to the broadcast.
 */
async function startLive(user: ReturnType<typeof userEvent.setup>) {
  localStorage.setItem('louver.warned', '1')
  await user.click(await screen.findByRole('button', { name: /방송 시작/ }))
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

  it('never asks an ordinary user for a Client ID or Secret', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')

    // §1/§7: the product ships its own OAuth client. Someone who just wants to
    // broadcast music must not meet the Google Cloud console.
    expect(await screen.findByRole('button', { name: 'YouTube 계정 연결' })).toBeInTheDocument()
    expect(screen.getByText('연결되지 않음')).toBeInTheDocument()
    expect(screen.queryByLabelText('OAuth 클라이언트 ID')).not.toBeInTheDocument()
    expect(screen.queryByLabelText('OAuth 클라이언트 보안 비밀')).not.toBeInTheDocument()
    expect(screen.queryByText(/고급: 자체 OAuth 클라이언트/)).not.toBeInTheDocument()
  })

  it('offers 계정 변경 once connected', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')

    const polled = { timeout: 6000 }
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    expect(await screen.findByRole('button', { name: '계정 변경' }, polled)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '연결 해제' })).toBeInTheDocument()
  })

  it('shows the consent address when a browser cannot be opened', async () => {
    const user = userEvent.setup()
    mount({ failOpener: true })
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')

    // The consent server is already listening at this point, so a browser that
    // will not open must not throw away the attempt.
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    expect(await screen.findByText(/브라우저를 열지 못했습니다/)).toBeInTheDocument()
    expect(screen.getByText(/accounts\.google\.com/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '주소 복사' })).toBeInTheDocument()
    expect(screen.queryByText('알 수 없는 오류가 발생했습니다.')).not.toBeInTheDocument()
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

describe('broadcasting without a Google account', () => {
  it('puts the stream key first and marks the YouTube card optional', async () => {
    const user = userEvent.setup()
    mount({ hasStreamKey: false })
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')

    // The basic card is what a broadcast actually needs, so it comes first and
    // says out loud that nothing else is required.
    const basic = await screen.findByRole('heading', { name: '기본 송출' })
    expect(screen.getByText(/방송에 필요한 것은 스트림 키 하나뿐입니다/)).toBeInTheDocument()
    expect(screen.getByLabelText('YouTube 스트림 키')).toBeInTheDocument()

    const advanced = screen.getByRole('heading', { name: 'YouTube 고급 기능 (선택)' })
    expect(screen.getByText(/선택 기능입니다\. 방송만 사용하려면 연결할 필요가 없습니다\./))
      .toBeInTheDocument()

    // Order matters as much as wording: the optional card must not be the
    // first thing a new user meets.
    expect(basic.compareDocumentPosition(advanced))
      .toBe(Node.DOCUMENT_POSITION_FOLLOWING)
  })

  it('goes live with nothing but a stream key, never connecting an account', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    expect((backend('youtube_status', {}) as { connected: boolean }).connected).toBe(false)

    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())

    await gotoPage(user, '대시보드')
    await startLive(user)

    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })
    // Three steps — 영상 추가, 스트림 키, 방송 시작 — and no Google consent in
    // between. The account is still disconnected on a live stream.
    expect((backend('youtube_status', {}) as { connected: boolean }).connected).toBe(false)
    expect(screen.queryByText(/Google/)).not.toBeInTheDocument()
    expect(screen.queryByText(/계정을 연결/)).not.toBeInTheDocument()
  })

  it('keeps the broadcast running when the YouTube side fails', async () => {
    const user = userEvent.setup()
    mount({ youtubeApiFails: true })
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())

    await gotoPage(user, '대시보드')
    await startLive(user)
    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })

    // Connect the optional half, then let its API fail. §12: metadata and chat
    // are the only casualties — FFmpeg is not one of them.
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })

    await gotoPage(user, '방송 설정')
    await user.click(await screen.findByRole('button', { name: /지금 YouTube에 적용/ }))
    expect(await screen.findByText('LL-YOUTUBE-004')).toBeInTheDocument()
    await user.click(screen.getAllByRole('button', { name: '알림 닫기' })[0]!)

    await gotoPage(user, '대시보드')
    expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
  })
})

describe('the YouTube side of a broadcast is reported on its own', () => {
  /** Save a title so automatic apply has something to do. */
  async function saveTitle(user: ReturnType<typeof userEvent.setup>, title: string) {
    await gotoPage(user, '방송 설정')
    await user.type(await screen.findByLabelText('방송 제목'), title)
    await user.click(screen.getByRole('button', { name: '저장' }))
    await screen.findByText(/방송 정보를 저장했습니다/)
  }

  async function readyPlaylist(user: ReturnType<typeof userEvent.setup>) {
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())
  }

  it('does not claim settings will be applied when no account is connected', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '방송 설정')

    // §B-2: stream-key RTMPS cannot change a title, so the switch must not
    // say it will. Saying so is how a stream went live as "Playlist".
    expect(await screen.findByText(/방송 설정 자동 적용을 사용하려면 YouTube 계정 연결이 필요합니다/))
      .toBeInTheDocument()
    expect(screen.queryByText(/방송이 시작되면 저장된 제목·설명·태그/)).not.toBeInTheDocument()
    expect(screen.getByTestId('auto-apply-warning')).toBeInTheDocument()
  })

  it('promises the apply only once an account is there to do it with', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })

    await gotoPage(user, '방송 설정')
    expect(await screen.findByText(/방송이 시작되면 저장된 제목·설명·태그/)).toBeInTheDocument()
    expect(screen.queryByTestId('auto-apply-warning')).not.toBeInTheDocument()
  })

  it('asks before going live under whatever title YouTube already has', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)

    // §B-9: not a silent start. The user is told, and chooses.
    expect(await screen.findByText('방송 설정을 YouTube에 적용하지 못했습니다')).toBeInTheDocument()
    expect(screen.getByText(/방송 설정 자동 적용을 사용하려면 YouTube 계정 연결이 필요합니다/))
      .toBeInTheDocument()
    // Connecting an account is what actually fixes this; pressing the same
    // button again would only repeat the same refusal.
    expect(screen.getByRole('button', { name: 'YouTube 연결' })).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: '다시 시도' })).not.toBeInTheDocument()
    expect(screen.getAllByTestId('status-pill')[0]).not.toHaveAttribute('data-state', 'LIVE')

    // Taking the offer starts the stream, and only the stream.
    await user.click(screen.getByRole('button', { name: '설정 없이 방송 시작' }))
    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })
  })

  it('does not turn the whole dashboard red over a title', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)
    await screen.findByText('방송 설정을 YouTube에 적용하지 못했습니다')
    await user.click(screen.getByRole('button', { name: '취소' }))

    // The broadcast engine was never in trouble, so the stream state stays
    // OFFLINE — not the red ERROR a failed FFmpeg would produce.
    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).not.toHaveAttribute('data-state', 'ERROR')
    })
    expect(screen.queryByTestId('start-error')).not.toBeInTheDocument()

    // The YouTube half keeps its own state, and says what to do about it.
    expect(await screen.findByTestId('metadata-action-required')).toBeInTheDocument()
    expect(screen.getByText('조치 필요')).toBeInTheDocument()
  })

  it('sends YouTube 연결 to the settings page instead of retrying', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)
    await user.click(await screen.findByRole('button', { name: 'YouTube 연결' }))

    expect(await screen.findByRole('heading', { name: 'YouTube 고급 기능 (선택)' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'YouTube 계정 연결' })).toBeInTheDocument()
  })

  it('says 적용 안 함 for a broadcast the user chose to start without settings', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)
    await user.click(await screen.findByRole('button', { name: '설정 없이 방송 시작' }))
    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })

    // The refusal is cleared and replaced by what the user actually decided:
    // not an outstanding problem, and not a claim that anything was applied.
    const panel = await screen.findByTestId('metadata-skipped')
    expect(within(panel).getByText(/YouTube에 저장된 기본 설정을 그대로 사용합니다/)).toBeInTheDocument()
    expect(screen.getByText('적용 안 함')).toBeInTheDocument()
    expect(screen.queryByTestId('metadata-action-required')).not.toBeInTheDocument()
    expect(screen.queryByText('조치 필요')).not.toBeInTheDocument()
  })

  it('reports each field from what YouTube says afterwards, not from the status code', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)
    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })

    // §B-10: a row per field, each one an answer to "did this take".
    const report = await screen.findByTestId('metadata-report')
    for (const field of ['제목', '설명', '태그', '카테고리', '공개범위']) {
      expect(within(report).getByText(field)).toBeInTheDocument()
    }
    expect(within(report).getAllByText('적용 완료').length).toBe(5)
    expect(within(report).queryByText('적용 실패')).not.toBeInTheDocument()
  })

  it('says 적용 실패 when YouTube keeps its own title despite a 200', async () => {
    const user = userEvent.setup()
    // The reported failure exactly: the calls succeed, the watch page still
    // reads "Playlist".
    mount({ youtubeIgnoresTitle: true })
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)

    const report = await screen.findByTestId('metadata-report')
    expect(within(report).getByText('적용 실패')).toBeInTheDocument()
    // And it says what YouTube actually has, which is the useful part.
    expect(within(report).getByText(/현재 Playlist/)).toBeInTheDocument()
  })

  it('pauses the optional half when the free quota runs out, and broadcasts anyway', async () => {
    const user = userEvent.setup()
    mount({ youtubeQuotaExhausted: true })
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)

    // The allowance is not something the user can fix and not a reason to
    // take a channel off air: no dialog, and the stream starts.
    expect(screen.queryByText('방송 설정을 YouTube에 적용하지 못했습니다')).not.toBeInTheDocument()
    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })

    const panel = await screen.findByTestId('metadata-quota')
    expect(within(panel).getByText(/무료 API 사용량이 소진되었습니다/)).toBeInTheDocument()
    expect(within(panel).getByText(/다음 초기화 후 다시 사용할 수 있습니다/)).toBeInTheDocument()
    expect(within(panel).getByText(/영상 방송에는 영향을 주지 않습니다/)).toBeInTheDocument()
    // Stated as a consequence of the operating policy, not as a claim about
    // Google's pricing.
    expect(within(panel).getByText(/유료 Google Cloud 서비스를 사용하지 않으므로/)).toBeInTheDocument()
    expect(screen.getByText('오늘 사용량 초과')).toBeInTheDocument()
  })

  it('shows what the day has cost, and that it costs nothing', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })

    // The final UX §: channel, a connected dot, and the two buttons.
    expect(screen.getByText('ROOM.')).toBeInTheDocument()
    expect(screen.getByText('연결됨')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '계정 변경' })).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '연결 해제' })).toBeInTheDocument()
    expect(await screen.findByText(/무료 한도 내/)).toBeInTheDocument()
  })

  it('keeps FFmpeg alive when the metadata call fails after the user opts to start anyway', async () => {
    const user = userEvent.setup()
    mount({ youtubeApiFails: true })
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)
    // The heading names the step that failed, not the feature that wanted it.
    expect(await screen.findByText('방송 정보 적용 실패')).toBeInTheDocument()
    await user.click(screen.getByRole('button', { name: '설정 없이 방송 시작' }))

    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })
    // §12 still holds: the stream is not a casualty of the YouTube half.
    await new Promise((r) => setTimeout(r, 1200))
    expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
  })

  it('names the step that failed rather than repeating one sentence for all of them', async () => {
    // The real Mac failure this replaces: six retries, six identical
    // "YouTube에 연결하지 못했습니다" lines, and no way to tell which of the
    // seven requests Google refused.
    const user = userEvent.setup()
    mount({ youtubeApiFails: true, youtubeFailsAt: 'broadcast_insert' })
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)

    expect(await screen.findByText('예약 방송 생성 실패')).toBeInTheDocument()
    // And Google's own words are there to act on, not summarised away.
    expect(screen.getByTestId('blocked-detail')).toHaveTextContent('예약 방송을 만들지 못했습니다')
    expect(screen.getByTestId('blocked-google')).toHaveTextContent('liveBroadcasts.insert')
    expect(screen.getByTestId('blocked-google')).toHaveTextContent('reason=liveStreamingNotEnabled')

    // The panel behind the modal says the same thing, with the remedy and the
    // sequence it got through.
    await user.click(screen.getByRole('button', { name: '취소' }))
    expect(await screen.findByTestId('metadata-failed-stage')).toHaveTextContent('예약 방송 생성 실패')
    expect(screen.getByTestId('metadata-failed')).toHaveTextContent('실시간 스트리밍이 사용 설정')
    await user.click(screen.getByText('상세정보'))
    const steps = await screen.findByTestId('metadata-steps')
    expect(steps).toHaveTextContent('Google 인증 갱신')
    expect(steps).toHaveTextContent('예약 방송 확인')
    expect(steps).toHaveTextContent('예약 방송 생성')
  })

  it('does not call a refused request a lost connection', async () => {
    // The failure real Google actually sent: `liveBroadcasts.list` with two
    // filters. The account is connected and the token was refreshed a line
    // earlier, so "YouTube에 연결하지 못했습니다" would send the user to check
    // their internet and reconnect — neither of which is the problem.
    const user = userEvent.setup()
    mount({ youtubeApiFails: true, youtubeFailsAt: 'broadcast_list' })
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)

    expect(await screen.findByText('예약 방송 목록 조회 실패')).toBeInTheDocument()
    expect(screen.getByTestId('blocked-detail')).toHaveTextContent('예약 방송 정보를 조회하지 못했습니다')
    expect(screen.queryByText(/YouTube에 연결하지 못했습니다/)).not.toBeInTheDocument()
    // Google's own words stay available for whoever has to fix it.
    await user.click(screen.getByRole('button', { name: '취소' }))
    await user.click(await screen.findByText('상세정보'))
    expect(screen.getByTestId('metadata-failed')).toHaveTextContent('incompatibleParameters')
  })

  it('tells a stale Google login apart from a YouTube API failure', async () => {
    // §3: the refresh path is what a scheduled start at 03:00 depends on, and
    // a manual broadcast an hour earlier proves nothing about it — that run
    // was still holding the access token consent had just minted.
    const user = userEvent.setup()
    mount({ youtubeApiFails: true, youtubeFailsAt: 'token_refresh' })
    await screen.findByRole('button', { name: '대시보드' })
    await saveTitle(user, 'COLORIST 24시간 편집샵 느낌 플레이리스트')
    await gotoPage(user, '설정')
    await user.click(await screen.findByRole('button', { name: 'YouTube 계정 연결' }))
    await screen.findByRole('button', { name: '연결 해제' }, { timeout: 6000 })
    await readyPlaylist(user)

    await gotoPage(user, '대시보드')
    await startLive(user)

    expect(await screen.findByText('Google 인증 갱신 실패')).toBeInTheDocument()
    expect(screen.getByTestId('blocked-detail')).toHaveTextContent('Google 인증 갱신에 실패했습니다')
    expect(screen.getByTestId('blocked-google')).toHaveTextContent('oauth2.token(refresh_token)')
    expect(screen.getByTestId('blocked-google')).toHaveTextContent('invalid_grant')

    // And the broadcast engine is still not the thing that is unwell: the
    // user can go on air without the optional half.
    await user.click(screen.getByRole('button', { name: '설정 없이 방송 시작' }))
    await waitFor(() => {
      expect(screen.getAllByTestId('status-pill')[0]).toHaveAttribute('data-state', 'LIVE')
    })
  })
})

describe('the schedule list', () => {
  async function makeSchedule(user: ReturnType<typeof userEvent.setup>) {
    await gotoPage(user, '방송 예약')
    await user.clear(await screen.findByLabelText('시작 시간'))
    await user.type(screen.getByLabelText('시작 시간'), '17:14')
    await user.clear(screen.getByLabelText('종료 시간'))
    await user.type(screen.getByLabelText('종료 시간'), '17:40')
    await user.click(screen.getByRole('button', { name: /예약 추가/ }))
    await screen.findByText('17:14 → 17:40')
  }

  it('says the window is open now rather than announcing tomorrow', async () => {
    const user = userEvent.setup()
    // Armed, because an open window only means anything when this computer
    // is watching the clock.
    mount({ scheduleActiveNow: true, schedulerArmed: true })
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await makeSchedule(user)

    // The reported confusion: at 17:17, a 17:14→17:40 schedule showed only
    // "다음 방송 <tomorrow>", which reads as a window that was skipped.
    const row = await screen.findByText(/지금 방송 시간입니다/)
    expect(row).toBeInTheDocument()
    // The row itself must not still be announcing tomorrow. The scheduler
    // panel above legitimately names the *next* window; this is about the row.
    expect(within(row.closest('li')!).queryByText(/다음 방송/)).not.toBeInTheDocument()
  })

  it('still shows the next repeat when no window is open', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await makeSchedule(user)

    expect(await screen.findByText(/다음 방송 내일 17:14/)).toBeInTheDocument()
    expect(screen.queryByText(/지금 방송 시간입니다/)).not.toBeInTheDocument()
  })
})

describe('an open scheduled window', () => {
  it('is reported on the dashboard even before anything is broadcasting', async () => {
    // §8: the reported contradiction — the schedule page saying the window is
    // open while the dashboard said there was no scheduled broadcast at all.
    mount({
      schedulerArmed: true,
      occurrenceOpen: { start: '2026-09-20 20:58', end: '2026-09-20 21:00', phase: 'preparing' },
    })
    await screen.findByRole('button', { name: '대시보드' })

    const row = await screen.findByTestId('active-occurrence')
    expect(within(row).getByText(/20:58/)).toBeInTheDocument()
    expect(within(row).getByText(/21:00/)).toBeInTheDocument()
    expect(within(row).getByText(/시작 준비 중/)).toBeInTheDocument()
    expect(screen.queryByText('예약된 방송이 없습니다')).not.toBeInTheDocument()
  })

  it('says when the next attempt is, rather than looking idle', async () => {
    mount({
      schedulerArmed: true,
      occurrenceOpen: { start: '2026-09-20 20:58', end: '2026-09-20 21:10', phase: 'preparing', retryInSecs: 5 },
    })
    await screen.findByRole('button', { name: '대시보드' })

    // §9: a window that failed once is retrying inside itself, not waiting
    // for tomorrow, and the screen says so.
    const row = await screen.findByTestId('active-occurrence')
    expect(within(row).getByText(/5초 후 재시도/)).toBeInTheDocument()
    expect(within(row).getByText(/1회째/)).toBeInTheDocument()
  })

  it('reads LIVE once the broadcast is running', async () => {
    const user = userEvent.setup()
    mount({
      schedulerArmed: true,
      occurrenceOpen: { start: '2026-09-20 20:58', end: '2026-09-20 21:00', phase: 'preparing' },
    })
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await user.click(screen.getByRole('button', { name: /방송용으로 최적화/ }))
    await user.click(await screen.findByRole('button', { name: '최적화 시작' }))
    await waitFor(() => expect(screen.queryByText(/방송 규격과 다릅니다/)).not.toBeInTheDocument())

    await gotoPage(user, '대시보드')
    await startLive(user)
    await waitFor(() => {
      expect(within(screen.getByTestId('active-occurrence')).getByText('LIVE')).toBeInTheDocument()
    })
  })
})

describe('the scheduler is a switch, not a saved rule', () => {
  async function addSchedule(user: ReturnType<typeof userEvent.setup>) {
    await gotoPage(user, '방송 예약')
    await user.clear(await screen.findByLabelText('시작 시간'))
    await user.type(screen.getByLabelText('시작 시간'), '20:58')
    await user.clear(screen.getByLabelText('종료 시간'))
    await user.type(screen.getByLabelText('종료 시간'), '21:00')
    await user.click(screen.getByRole('button', { name: /예약 추가/ }))
    await screen.findByText('20:58 → 21:00')
  }

  it('says the rule is saved but nothing is watching it yet', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await addSchedule(user)

    // §8: "예약을 추가했습니다" alone is what left the user wondering whether
    // anything would actually happen tonight.
    expect(await screen.findByText(/자동 방송을 사용하려면/)).toBeInTheDocument()
    const panel = screen.getByTestId('scheduler-state')
    expect(within(panel).getByText('꺼짐')).toBeInTheDocument()
    expect(within(panel).getByText(/자동 방송은 꺼져 있습니다/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: '예약 방송 시작' })).toBeInTheDocument()
  })

  it('switches on, and then proves it is really waiting', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await addSchedule(user)

    await user.click(screen.getByRole('button', { name: '예약 방송 시작' }))

    const panel = await screen.findByTestId('scheduler-state')
    expect(within(panel).getByText('예약 대기 중')).toBeInTheDocument()
    // A countdown that moves is the difference between a claim and a fact.
    expect(await screen.findByTestId('scheduler-countdown')).toHaveTextContent(/\d\d:\d\d:\d\d 후 자동 시작/)
    expect(screen.getByRole('button', { name: '예약 방송 중지' })).toBeInTheDocument()
  })

  it('refuses to switch on when there is nothing to watch', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await gotoPage(user, '방송 예약')

    await user.click(await screen.findByRole('button', { name: '예약 방송 시작' }))
    // Checked now rather than at 3am, when there is nobody to tell.
    expect(await screen.findByText(/사용 중인 예약이 없습니다/)).toBeInTheDocument()
  })

  it('keeps the schedules when it is switched off', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await addSchedule(user)
    await user.click(screen.getByRole('button', { name: '예약 방송 시작' }))
    await screen.findByRole('button', { name: '예약 방송 중지' })

    await user.click(screen.getByRole('button', { name: '예약 방송 중지' }))
    expect(await screen.findByText(/예약은 그대로 저장되어 있습니다/)).toBeInTheDocument()
    expect(screen.getByText('20:58 → 21:00')).toBeInTheDocument()
    expect(within(screen.getByTestId('scheduler-state')).getByText('꺼짐')).toBeInTheDocument()
  })

  it('does not call an open window a broadcast when nothing is watching', async () => {
    const user = userEvent.setup()
    // The window's time has come and the scheduler is off.
    mount({ occurrenceOpen: { start: '2026-09-20 20:58', end: '2026-09-20 21:00', phase: 'preparing' } })
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await addSchedule(user)

    expect(await screen.findByText(/지금이 예약 시간이지만 자동 방송이 꺼져 있습니다/)).toBeInTheDocument()
    expect(screen.queryByText(/지금 방송 시간입니다/)).not.toBeInTheDocument()
  })

  it('tells the dashboard that a stopped FFmpeg is the correct state', async () => {
    mount({ schedulerArmed: true })
    await screen.findByRole('button', { name: '대시보드' })

    // §10: with the scheduler waiting, FFmpeg being stopped is not a fault.
    const row = await screen.findByTestId('scheduler-waiting')
    expect(within(row).getByText(/예약 방송 대기 중/)).toBeInTheDocument()
    expect(within(row).getByText(/예약 시간에 자동으로 시작합니다/)).toBeInTheDocument()
  })

  it('labels the per-row toggle so it is not mistaken for the switch', async () => {
    const user = userEvent.setup()
    mount()
    await screen.findByRole('button', { name: '대시보드' })
    await buildPlaylist(user)
    await addSchedule(user)

    // §9: a green toggle on a row means "use this rule", not "broadcasting".
    expect(screen.getByRole('switch', { name: '이 예약 사용' })).toBeInTheDocument()
  })
})
