/**
 * YouTube ingest targets.
 *
 * The key is typed once and sent once. It is not written to any store, not put
 * in a URL, and not read back: what the list shows is the mask the server
 * returns, because the server has no endpoint that would return anything else
 * (§9).
 */
import { useEffect, useState } from 'react'
import { Button, Card, EmptyState, Field, Input, Modal } from '@/components/ui'
import { useTransport } from '../TransportContext'
import type { StreamDestination } from '../cloud'

const YOUTUBE_INGEST = 'rtmps://a.rtmps.youtube.com/live2'

export function Destinations() {
  const t = useTransport()
  const [items, setItems] = useState<StreamDestination[]>([])
  const [adding, setAdding] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function refresh() {
    try {
      setItems(await t.listDestinations())
    } catch {
      /* shown by the next action that fails */
    }
  }

  useEffect(() => {
    refresh()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [t])

  return (
    <Card
      title="송출 대상"
      action={
        <Button variant="primary" size="sm" onClick={() => setAdding(true)}>
          대상 추가
        </Button>
      }
    >
      {error && (
        <p role="alert" className="mb-3 text-sm text-live">
          {error}
        </p>
      )}
      {items.length === 0 ? (
        <EmptyState title="송출 대상이 없습니다" hint="YouTube Live의 스트림 키를 추가하세요." />
      ) : (
        <ul className="divide-y divide-ink-700">
          {items.map((d) => (
            <li key={d.id} className="flex items-center justify-between gap-4 py-3" data-testid="destination-row">
              <div className="min-w-0">
                <div className="truncate text-sm text-ink-100">{d.label}</div>
                <div className="mt-1 font-mono text-xs text-ink-500">
                  {d.rtmps_url} · <span data-testid="masked-key">{d.key_masked}</span>
                </div>
              </div>
              <Button
                size="sm"
                onClick={async () => {
                  setError(null)
                  try {
                    await t.deleteDestination(d.id)
                    await refresh()
                  } catch (e) {
                    setError(e instanceof Error ? e.message : '삭제할 수 없습니다.')
                  }
                }}
              >
                삭제
              </Button>
            </li>
          ))}
        </ul>
      )}
      <AddDestination
        open={adding}
        onClose={() => setAdding(false)}
        onAdded={async () => {
          setAdding(false)
          await refresh()
        }}
      />
    </Card>
  )
}

function AddDestination({
  open, onClose, onAdded,
}: { open: boolean; onClose: () => void; onAdded: () => void }) {
  const t = useTransport()
  const [label, setLabel] = useState('')
  const [url, setUrl] = useState(YOUTUBE_INGEST)
  const [key, setKey] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function save() {
    setBusy(true)
    setError(null)
    try {
      await t.createDestination({ label: label.trim() || '내 채널', rtmps_url: url.trim(), stream_key: key.trim() })
      // Gone from this tab's memory as soon as the server has it.
      setKey('')
      setLabel('')
      onAdded()
    } catch (e) {
      setError(e instanceof Error ? e.message : '저장할 수 없습니다.')
    } finally {
      setBusy(false)
    }
  }

  return (
    <Modal
      open={open}
      title="송출 대상 추가"
      onClose={onClose}
      footer={
        <>
          <Button onClick={onClose}>취소</Button>
          <Button variant="primary" onClick={save} disabled={busy || key.trim().length === 0}>
            저장
          </Button>
        </>
      }
    >
      <Field label="이름">
        <Input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="내 채널" />
      </Field>
      <Field label="서버 주소">
        <Input value={url} onChange={(e) => setUrl(e.target.value)} />
      </Field>
      <Field label="스트림 키" hint="저장한 뒤에는 다시 볼 수 없습니다. 서버에 암호화되어 보관됩니다.">
        <Input
          type="password"
          autoComplete="off"
          value={key}
          onChange={(e) => setKey(e.target.value)}
          aria-label="스트림 키"
        />
      </Field>
      {error && (
        <p role="alert" className="pt-2 text-sm text-live">
          {error}
        </p>
      )}
    </Modal>
  )
}
