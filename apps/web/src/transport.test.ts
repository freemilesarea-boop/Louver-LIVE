/**
 * What the web transport does, and what it must never do.
 *
 * The "never" half is the point: a session token or a stream key that reaches
 * browser storage is a leak no later code can undo, so these tests watch the
 * storages as well as the requests.
 */
import { describe, expect, it, vi } from 'vitest'
import { HttpError, WebTransport } from './transport'

type Call = { url: string; init: RequestInit }

function stubFetch(reply: (call: Call) => { status?: number; body?: unknown }) {
  const calls: Call[] = []
  vi.stubGlobal('fetch', (url: string, init: RequestInit = {}) => {
    calls.push({ url, init })
    const { status = 200, body = null } = reply({ url, init })
    return Promise.resolve(
      new Response(body === null ? '' : JSON.stringify(body), {
        status,
        headers: { 'Content-Type': 'application/json' },
      }),
    )
  })
  return calls
}

describe('WebTransport', () => {
  it('sends the session cookie and stores no token of its own', async () => {
    const calls = stubFetch(() => ({ body: { id: 'u1', email: 'a@b.com', plan_id: 'basic' } }))
    const t = new WebTransport()

    const me = await t.login('a@b.com', 'correct-horse-battery')

    expect(me.email).toBe('a@b.com')
    expect(calls[0]?.url).toBe('/api/auth/login')
    // Without this the HttpOnly cookie never travels and every call is a 401.
    expect(calls[0]?.init.credentials).toBe('include')
    // And the token itself is the server's business: there is nothing here that
    // an injected script could read.
    expect(localStorage.length).toBe(0)
    expect(sessionStorage.length).toBe(0)
    expect(document.cookie).toBe('')
  })

  it('never keeps a stream key, and reads back only a mask', async () => {
    const calls = stubFetch(() => ({
      body: {
        id: 'd1',
        user_id: 'u1',
        label: '내 채널',
        rtmps_url: 'rtmps://a.rtmps.youtube.com/live2',
        key_masked: '••••••••••••',
        created_at: '',
      },
    }))
    const t = new WebTransport()

    const saved = await t.createDestination({
      label: '내 채널',
      rtmps_url: 'rtmps://a.rtmps.youtube.com/live2',
      stream_key: 'abcd-1234-efgh-5678',
    })

    // It goes out once, in the body, and comes back as dots.
    expect(calls[0]?.init.body).toContain('abcd-1234-efgh-5678')
    expect(saved.key_masked).toBe('••••••••••••')
    expect('key' in saved).toBe(false)
    expect(localStorage.length).toBe(0)
    expect(sessionStorage.length).toBe(0)
    // Not in the URL either, where it would reach every proxy log on the way.
    expect(calls[0]?.url).not.toContain('abcd')
  })

  it("passes the server's own refusal through, with a code the UI can branch on", async () => {
    stubFetch(() => ({
      status: 402,
      body: { error: 'max_concurrent_streams 한도를 초과했습니다 (1/1)' },
    }))
    const t = new WebTransport()

    await expect(t.startBroadcast('b1')).rejects.toBeInstanceOf(HttpError)
    await expect(t.startBroadcast('b1')).rejects.toMatchObject({
      status: 402,
      louver: { code_str: 'LL-PLAN', message: 'max_concurrent_streams 한도를 초과했습니다 (1/1)' },
    })
  })

  it('turns a dead server into a message, not an unhandled rejection', async () => {
    vi.stubGlobal('fetch', () => Promise.reject(new Error('ECONNREFUSED')))
    const t = new WebTransport()
    await expect(t.dashboard()).rejects.toMatchObject({
      status: 0,
      louver: { code_str: 'LL-NET' },
    })
  })

  it('reports upload progress and does not swallow a refusal', async () => {
    class FakeXhr {
      status = 0
      responseText = ''
      withCredentials = false
      upload = { onprogress: null as ((e: ProgressEvent) => void) | null }
      onload: (() => void) | null = null
      onerror: (() => void) | null = null
      open() {}
      send() {
        this.upload.onprogress?.({ lengthComputable: true, loaded: 5, total: 10 } as ProgressEvent)
        this.status = 402
        this.responseText = JSON.stringify({ error: 'max_upload_bytes 한도를 초과했습니다 (4096/64)' })
        this.onload?.()
      }
    }
    vi.stubGlobal('XMLHttpRequest', FakeXhr)

    const seen: number[] = []
    const t = new WebTransport()
    await expect(
      t.uploadMedia(new File(['x'], 'big.mp4', { type: 'video/mp4' }), (f) => seen.push(f)),
    ).rejects.toMatchObject({ status: 402, louver: { code_str: 'LL-PLAN' } })
    expect(seen).toEqual([0.5])
  })

  it('subscribes to the dashboard over SSE and hands back an unsubscribe', async () => {
    const closed: boolean[] = []
    class FakeSource {
      listeners: Record<string, (e: MessageEvent<string>) => void> = {}
      constructor(readonly url: string, readonly init?: { withCredentials?: boolean }) {}
      addEventListener(name: string, cb: (e: MessageEvent<string>) => void) {
        this.listeners[name] = cb
      }
      close() {
        closed.push(true)
      }
    }
    let made: FakeSource | null = null
    vi.stubGlobal('EventSource', function (url: string, init?: { withCredentials?: boolean }) {
      made = new FakeSource(url, init)
      return made
    })

    const t = new WebTransport()
    const seen: number[] = []
    const stop = t.watchDashboard((d) => seen.push(d.active))

    expect(made!.url).toBe('/api/events')
    expect(made!.init?.withCredentials).toBe(true)
    made!.listeners.dashboard?.({
      data: JSON.stringify({ plan_label: 'Pro', active: 2, allowed: 3, broadcasts: [] }),
    } as MessageEvent<string>)
    // A truncated frame must not take the page down with it.
    made!.listeners.dashboard?.({ data: '{"plan_' } as MessageEvent<string>)
    expect(seen).toEqual([2])

    stop()
    expect(closed).toEqual([true])
  })
})
