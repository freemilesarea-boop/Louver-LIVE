/**
 * The one seam between the UI and whatever is behind it (§11).
 *
 * Components never call `fetch`, never call `invoke`, and never look at an
 * environment variable. They take a {@link Transport} from context, so the
 * difference between "this is running in the desktop app" and "this is running
 * in a browser against a server" lives in exactly two classes in this file.
 */
import type {
  Broadcast, BroadcastEvent, CloudMedia, Dashboard, Me, NewBroadcast, NewDestination,
  StreamDestination, Subscription,
} from './cloud'
import type { LouverError } from '@/types'

export interface Transport {
  /** Which kind of backend this is, for the few places that must say so. */
  readonly kind: 'web' | 'desktop'

  register(email: string, password: string): Promise<Me>
  login(email: string, password: string): Promise<Me>
  logout(): Promise<void>
  me(): Promise<Me>
  subscription(): Promise<Subscription>

  listMedia(): Promise<CloudMedia[]>
  uploadMedia(file: File, onProgress?: (fraction: number) => void): Promise<CloudMedia>
  deleteMedia(id: string): Promise<void>

  listDestinations(): Promise<StreamDestination[]>
  createDestination(input: NewDestination): Promise<StreamDestination>
  deleteDestination(id: string): Promise<void>

  dashboard(): Promise<Dashboard>
  createBroadcast(input: NewBroadcast): Promise<Broadcast>
  startBroadcast(id: string): Promise<Broadcast>
  stopBroadcast(id: string): Promise<Broadcast>
  restartBroadcast(id: string): Promise<Broadcast>
  deleteBroadcast(id: string): Promise<void>
  logs(id: string, limit?: number): Promise<BroadcastEvent[]>

  /** Live dashboard updates. Returns an unsubscribe function. */
  watchDashboard(onSnapshot: (d: Dashboard) => void): () => void
}

/** Every rejection reaches the UI in the shape its error banner already knows. */
function asLouverError(code: string, message: string, detail?: string): LouverError {
  return { code_str: code, message, detail }
}

export class HttpError extends Error {
  constructor(
    readonly status: number,
    readonly louver: LouverError,
  ) {
    super(louver.message)
  }
}

/**
 * Talks to the Louver server.
 *
 * The session lives in an `HttpOnly` cookie the server sets, so there is
 * nothing here that reads or writes a token — `credentials: 'include'` is the
 * whole of it. That is deliberate: a token this code could read is a token an
 * injected script could read too.
 */
export class WebTransport implements Transport {
  readonly kind = 'web' as const

  constructor(private readonly base = '') {}

  private async json<T>(path: string, init: RequestInit = {}): Promise<T> {
    let res: Response
    try {
      res = await fetch(`${this.base}${path}`, {
        credentials: 'include',
        ...init,
        headers: { ...(init.body ? { 'Content-Type': 'application/json' } : {}), ...init.headers },
      })
    } catch (e) {
      throw new HttpError(
        0,
        asLouverError('LL-NET', '서버에 연결할 수 없습니다.', e instanceof Error ? e.message : undefined),
      )
    }
    return this.unwrap<T>(res)
  }

  private async unwrap<T>(res: Response): Promise<T> {
    const text = await res.text()
    if (!res.ok) {
      let message = '요청을 처리할 수 없습니다.'
      try {
        const body = JSON.parse(text) as { error?: string }
        if (body.error) message = body.error
      } catch {
        /* a proxy's HTML error page; the status is what matters */
      }
      throw new HttpError(res.status, asLouverError(codeFor(res.status), message))
    }
    return (text ? JSON.parse(text) : undefined) as T
  }

  register(email: string, password: string) {
    return this.json<Me>('/api/auth/register', {
      method: 'POST',
      body: JSON.stringify({ email, password }),
    })
  }

  login(email: string, password: string) {
    return this.json<Me>('/api/auth/login', {
      method: 'POST',
      body: JSON.stringify({ email, password }),
    })
  }

  async logout() {
    await this.json<void>('/api/auth/logout', { method: 'POST' })
  }

  me() {
    return this.json<Me>('/api/me')
  }

  subscription() {
    return this.json<Subscription>('/api/me/subscription')
  }

  listMedia() {
    return this.json<CloudMedia[]>('/api/media')
  }

  /**
   * Uploads with `XMLHttpRequest`, not `fetch`, for one reason: a progress
   * event. A 4 GB video with no visible progress looks broken.
   */
  uploadMedia(file: File, onProgress?: (fraction: number) => void): Promise<CloudMedia> {
    return new Promise((resolve, reject) => {
      const form = new FormData()
      form.append('file', file, file.name)
      const xhr = new XMLHttpRequest()
      xhr.open('POST', `${this.base}/api/media/upload`)
      xhr.withCredentials = true
      if (onProgress && xhr.upload) {
        xhr.upload.onprogress = (e) => {
          if (e.lengthComputable && e.total > 0) onProgress(e.loaded / e.total)
        }
      }
      xhr.onload = () => {
        if (xhr.status >= 200 && xhr.status < 300) {
          resolve(JSON.parse(xhr.responseText) as CloudMedia)
          return
        }
        let message = '업로드에 실패했습니다.'
        try {
          const body = JSON.parse(xhr.responseText) as { error?: string }
          if (body.error) message = body.error
        } catch {
          /* no JSON body */
        }
        reject(new HttpError(xhr.status, asLouverError(codeFor(xhr.status), message)))
      }
      xhr.onerror = () =>
        reject(new HttpError(0, asLouverError('LL-NET', '업로드 중 연결이 끊겼습니다.')))
      xhr.send(form)
    })
  }

  async deleteMedia(id: string) {
    await this.json<void>(`/api/media/${encodeURIComponent(id)}`, { method: 'DELETE' })
  }

  listDestinations() {
    return this.json<StreamDestination[]>('/api/stream-destinations')
  }

  /**
   * Sends the key once and keeps no copy.
   *
   * The response is built from the row, so what comes back is a mask. Nothing
   * in this class writes to `localStorage` — §9's rule is kept by there being
   * no code that could break it.
   */
  createDestination(input: NewDestination) {
    return this.json<StreamDestination>('/api/stream-destinations', {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  async deleteDestination(id: string) {
    await this.json<void>(`/api/stream-destinations/${encodeURIComponent(id)}`, {
      method: 'DELETE',
    })
  }

  dashboard() {
    return this.json<Dashboard>('/api/broadcasts')
  }

  createBroadcast(input: NewBroadcast) {
    return this.json<Broadcast>('/api/broadcasts', {
      method: 'POST',
      body: JSON.stringify(input),
    })
  }

  startBroadcast(id: string) {
    return this.json<Broadcast>(`/api/broadcasts/${encodeURIComponent(id)}/start`, { method: 'POST' })
  }

  stopBroadcast(id: string) {
    return this.json<Broadcast>(`/api/broadcasts/${encodeURIComponent(id)}/stop`, { method: 'POST' })
  }

  restartBroadcast(id: string) {
    return this.json<Broadcast>(`/api/broadcasts/${encodeURIComponent(id)}/restart`, {
      method: 'POST',
    })
  }

  async deleteBroadcast(id: string) {
    await this.json<void>(`/api/broadcasts/${encodeURIComponent(id)}`, { method: 'DELETE' })
  }

  logs(id: string, limit = 100) {
    return this.json<BroadcastEvent[]>(
      `/api/broadcasts/${encodeURIComponent(id)}/logs?limit=${limit}`,
    )
  }

  /**
   * Server-sent events, and a poll when the browser has no `EventSource`.
   *
   * SSE because the traffic is one-way, it survives a proxy that knows nothing
   * of upgrades, and the browser reconnects on its own — which matters when the
   * thing being watched is expected to run for weeks.
   */
  watchDashboard(onSnapshot: (d: Dashboard) => void): () => void {
    if (typeof EventSource === 'undefined') {
      const timer = setInterval(() => {
        this.dashboard().then(onSnapshot).catch(() => undefined)
      }, 3000)
      return () => clearInterval(timer)
    }
    const source = new EventSource(`${this.base}/api/events`, { withCredentials: true })
    source.addEventListener('dashboard', (e) => {
      try {
        onSnapshot(JSON.parse((e as MessageEvent<string>).data) as Dashboard)
      } catch {
        /* a half-written frame; the next one is two seconds away */
      }
    })
    return () => source.close()
  }
}

function codeFor(status: number): string {
  if (status === 401) return 'LL-AUTH'
  if (status === 402) return 'LL-PLAN'
  if (status === 404) return 'LL-NOTFOUND'
  return `LL-HTTP-${status}`
}
