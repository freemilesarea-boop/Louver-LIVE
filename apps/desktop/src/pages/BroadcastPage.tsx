import { useCallback, useEffect, useState } from 'react'
import { Check, ChevronDown, ChevronUp, Plus, Save, Send, Trash2, Upload, X } from 'lucide-react'
import { useAppStore } from '@/stores/useAppStore'
import { api } from '@/services/ipc'
import { Badge, Button, Card, Field, Input, Select, Toggle } from '@/components/ui'
import type {
  BroadcastMetadata, BroadcastPreset, ChatMessage, ChatSettings, ChatStatus, Privacy,
} from '@/types'

/** YouTube's own limits, mirrored so the UI can count down to them. */
const MAX_TITLE = 100
const MAX_DESCRIPTION = 5000
const MAX_TAGS_TOTAL = 500
const MAX_CHAT_TEXT = 200

const CATEGORIES: { id: string; label: string }[] = [
  { id: '10', label: '음악' },
  { id: '24', label: '엔터테인먼트' },
  { id: '22', label: '인물 및 블로그' },
  { id: '20', label: '게임' },
  { id: '26', label: '노하우/스타일' },
  { id: '27', label: '교육' },
  { id: '28', label: '과학기술' },
]

const PRIVACY: { id: Privacy; label: string }[] = [
  { id: 'public', label: '공개 (Public)' },
  { id: 'unlisted', label: '일부공개 (Unlisted)' },
  { id: 'private', label: '비공개 (Private)' },
]

const INTERVALS = [10, 15, 20, 30, 60]

/**
 * What a tag list costs against YouTube's 500-character budget.
 *
 * A tag containing a space is stored quoted and costs two more, which is why
 * this is not simply the sum of the lengths.
 */
function tagsCost(tags: string[]): number {
  return tags.reduce((n, t, i) => n + (i > 0 ? 1 : 0) + t.length + (t.includes(' ') ? 2 : 0), 0)
}

/** 방송 설정 — everything that would otherwise mean opening YouTube Studio. */
export function BroadcastPage() {
  const { reportError, toast, status } = useAppStore()

  const [meta, setMeta] = useState<BroadcastMetadata | null>(null)
  const [presets, setPresets] = useState<BroadcastPreset[]>([])
  const [presetName, setPresetName] = useState('')
  const [tagDraft, setTagDraft] = useState('')
  const [saving, setSaving] = useState(false)
  const [applying, setApplying] = useState(false)
  const [connected, setConnected] = useState(false)
  const [applyOnStart, setApplyOnStart] = useState(true)

  const [messages, setMessages] = useState<ChatMessage[]>([])
  const [chat, setChat] = useState<ChatSettings | null>(null)
  const [chatStatus, setChatStatus] = useState<ChatStatus | null>(null)
  const [newMessage, setNewMessage] = useState('')
  const [minInterval, setMinInterval] = useState(300)

  const load = useCallback(async () => {
    try {
      const [m, p, msgs, cs, yt, min] = await Promise.all([
        api.youtubeGetMetadata(), api.youtubeListPresets(), api.chatListMessages(),
        api.chatGetSettings(), api.youtubeStatus(), api.chatMinIntervalSecs(),
      ])
      setMeta(m); setPresets(p); setMessages(msgs); setChat(cs)
      setConnected(yt.connected); setMinInterval(min); setApplyOnStart(yt.apply_on_start)
    } catch (e) {
      reportError(e)
    }
  }, [reportError])

  useEffect(() => { void load() }, [load])

  // The bot's state is polled while something is on air, so the user can watch
  // it connect and send rather than wondering.
  useEffect(() => {
    const t = setInterval(() => {
      api.chatStatus().then(setChatStatus).catch(() => { /* bot not running */ })
    }, 3000)
    return () => clearInterval(t)
  }, [])

  if (!meta || !chat) return <p className="text-sm text-ink-500">불러오는 중…</p>

  const update = (patch: Partial<BroadcastMetadata>) => setMeta({ ...meta, ...patch })
  const titleLeft = MAX_TITLE - [...meta.title].length
  const descLeft = MAX_DESCRIPTION - [...meta.description].length
  const tagCost = tagsCost(meta.tags)

  function addTags(raw: string) {
    if (!meta) return
    const incoming = raw.split(',').map((t) => t.trim()).filter(Boolean)
    if (incoming.length === 0) return
    const seen = new Set(meta.tags.map((t) => t.toLowerCase()))
    const merged = [...meta.tags]
    for (const t of incoming) {
      if (!seen.has(t.toLowerCase())) { merged.push(t); seen.add(t.toLowerCase()) }
    }
    update({ tags: merged })
    setTagDraft('')
  }

  async function saveMetadata() {
    if (!meta) return
    setSaving(true)
    try {
      setMeta(await api.youtubeSaveMetadata(meta))
      toast({ kind: 'success', message: '방송 정보를 저장했습니다.' })
    } catch (e) { reportError(e) } finally { setSaving(false) }
  }

  async function applyNow() {
    setApplying(true)
    try {
      const b = await api.youtubeApplyMetadata()
      toast({ kind: 'success', message: `YouTube 방송에 적용했습니다: ${b.title}` })
    } catch (e) { reportError(e) } finally { setApplying(false) }
  }

  async function savePreset() {
    if (!meta) return
    try {
      setPresets(await api.youtubeSavePreset(presetName, meta))
      toast({ kind: 'success', message: `프리셋 '${presetName}'을 저장했습니다.` })
      setPresetName('')
    } catch (e) { reportError(e) }
  }

  function applyPreset(p: BroadcastPreset) {
    setMeta({
      title: p.title, description: p.description, tags: p.tags,
      category_id: p.category_id, privacy: p.privacy,
    })
    toast({ kind: 'info', message: `'${p.name}' 프리셋을 불러왔습니다. 저장을 눌러 적용하세요.` })
  }

  async function saveChat(next: ChatSettings) {
    try {
      setChat(await api.chatSaveSettings(next))
    } catch (e) { reportError(e); void load() }
  }

  async function move(index: number, delta: number) {
    const next = [...messages]
    const target = index + delta
    if (target < 0 || target >= next.length) return
    ;[next[index], next[target]] = [next[target]!, next[index]!]
    setMessages(next)
    try {
      setMessages(await api.chatReorderMessages(next.map((m) => m.id)))
    } catch (e) { reportError(e) }
  }

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h1 className="text-lg font-semibold text-ink-100">방송 설정</h1>
        {!connected && <Badge tone="warn">YouTube 계정 미연결</Badge>}
      </div>

      {!connected && (
        <p className="rounded-lg border border-warn-dim bg-warn-dim/10 p-3 text-xs text-warn">
          설정 → YouTube에서 계정을 연결하면 여기에서 입력한 정보를 실제 방송에 반영할 수 있습니다.
          연결 전에도 내용은 저장해 둘 수 있습니다.
        </p>
      )}

      <Card title="방송 정보">
        <Field label="제목" hint={`${titleLeft}자 남음`}>
          <Input
            value={meta.title}
            aria-label="방송 제목"
            maxLength={MAX_TITLE}
            placeholder="PLAYLIST for your room | lofi, chill, jazz mood"
            onChange={(e) => update({ title: e.target.value })}
          />
        </Field>

        <Field label="설명" hint={`${descLeft}자 남음`}>
          <textarea
            value={meta.description}
            aria-label="방송 설명"
            rows={6}
            maxLength={MAX_DESCRIPTION}
            onChange={(e) => update({ description: e.target.value })}
            className="w-full rounded-md border border-ink-700 bg-ink-900 px-3 py-2 text-sm text-ink-100 outline-none focus:border-ink-500"
          />
        </Field>

        <Field label="태그" hint={`${tagCost} / ${MAX_TAGS_TOTAL}자 · Enter 또는 쉼표로 추가`}>
          <div className="flex flex-wrap items-center gap-1.5 rounded-md border border-ink-700 bg-ink-900 p-2">
            {meta.tags.map((t) => (
              <span
                key={t}
                className="inline-flex items-center gap-1 rounded bg-ink-700 px-2 py-0.5 text-xs text-ink-100"
              >
                {t}
                <button
                  type="button"
                  aria-label={`${t} 태그 삭제`}
                  onClick={() => update({ tags: meta.tags.filter((x) => x !== t) })}
                  className="text-ink-400 hover:text-live"
                >
                  <X size={12} />
                </button>
              </span>
            ))}
            <input
              value={tagDraft}
              aria-label="태그 입력"
              placeholder={meta.tags.length ? '' : 'lofi, jazz, chill'}
              onChange={(e) => {
                if (e.target.value.endsWith(',')) addTags(e.target.value)
                else setTagDraft(e.target.value)
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') { e.preventDefault(); addTags(tagDraft) }
                if (e.key === 'Backspace' && !tagDraft && meta.tags.length) {
                  update({ tags: meta.tags.slice(0, -1) })
                }
              }}
              onBlur={() => addTags(tagDraft)}
              className="min-w-[8rem] flex-1 bg-transparent px-1 text-sm text-ink-100 outline-none"
            />
          </div>
          {tagCost > MAX_TAGS_TOTAL && (
            <p className="mt-1 text-xs text-live">
              태그 전체 길이가 한도를 넘었습니다. 공백이 있는 태그는 따옴표 2자를 더 씁니다.
            </p>
          )}
        </Field>

        <div className="grid gap-4 md:grid-cols-2">
          <Field label="카테고리">
            <Select
              value={meta.category_id}
              aria-label="카테고리"
              onChange={(e) => update({ category_id: e.target.value })}
            >
              {CATEGORIES.map((c) => <option key={c.id} value={c.id}>{c.label}</option>)}
            </Select>
          </Field>
          <Field label="공개범위">
            <Select
              value={meta.privacy}
              aria-label="공개범위"
              onChange={(e) => update({ privacy: e.target.value as Privacy })}
            >
              {PRIVACY.map((p) => <option key={p.id} value={p.id}>{p.label}</option>)}
            </Select>
          </Field>
        </div>

        <div className="mt-4 flex flex-wrap gap-2 border-t border-ink-700 pt-4">
          <Button variant="primary" onClick={() => void saveMetadata()} disabled={saving}>
            <span className="inline-flex items-center gap-2"><Save size={15} /> 저장</span>
          </Button>
          <Button onClick={() => void applyNow()} disabled={!connected || applying}>
            <span className="inline-flex items-center gap-2"><Upload size={15} /> 지금 YouTube에 적용</span>
          </Button>
        </div>
        <Toggle
          label="방송을 시작할 때 자동으로 적용"
          hint="방송이 시작되면 저장된 제목·설명·태그·카테고리·공개범위를 YouTube에 한 번 적용합니다."
          checked={applyOnStart}
          onChange={(v) => {
            setApplyOnStart(v)
            api.youtubeSetApplyOnStart(v).catch(reportError)
          }}
        />
      </Card>

      <Card title="프리셋">
        <div className="flex flex-wrap gap-2">
          {presets.length === 0 && (
            <p className="text-xs text-ink-500">저장된 프리셋이 없습니다. 아래에서 현재 내용을 저장해보세요.</p>
          )}
          {presets.map((p) => (
            <span key={p.id} className="inline-flex items-center gap-1 rounded border border-ink-700 px-2 py-1">
              <button type="button" className="text-sm text-ink-100 hover:text-ok" onClick={() => applyPreset(p)}>
                {p.name}
              </button>
              <button
                type="button"
                aria-label={`${p.name} 프리셋 삭제`}
                className="text-ink-500 hover:text-live"
                onClick={() => api.youtubeDeletePreset(p.id).then(setPresets).catch(reportError)}
              >
                <Trash2 size={12} />
              </button>
            </span>
          ))}
        </div>
        <div className="mt-3 flex gap-2 border-t border-ink-700 pt-3">
          <Input
            value={presetName}
            aria-label="프리셋 이름"
            placeholder="ROOM. 24/7"
            onChange={(e) => setPresetName(e.target.value)}
            onKeyDown={(e) => { if (e.key === 'Enter' && presetName.trim()) void savePreset() }}
          />
          <Button onClick={() => void savePreset()} disabled={!presetName.trim()}>
            현재 내용을 프리셋으로 저장
          </Button>
        </div>
      </Card>

      <Card title="자동 라이브 채팅">
        <Toggle
          label="자동 라이브 채팅"
          hint={`방송이 실제 LIVE가 되고 채팅이 열린 뒤에만 전송합니다. 최소 간격 ${minInterval / 60}분.`}
          checked={chat.enabled}
          onChange={(v) => void saveChat({ ...chat, enabled: v })}
        />

        {chatStatus && chatStatus.state !== 'IDLE' && (
          <div className="mt-3 flex flex-wrap items-center gap-x-4 gap-y-1 rounded border border-ink-700 bg-ink-800/40 px-3 py-2 text-xs">
            <span className="font-mono text-ink-400">{chatStatus.state}</span>
            <span className="text-ink-200">{chatStatus.state_label}</span>
            <span className="text-ink-500">보낸 메시지 {chatStatus.messages_sent}개</span>
            {chatStatus.seconds_until_next != null && (
              <span className="text-ink-500">다음 전송까지 {Math.ceil(chatStatus.seconds_until_next / 60)}분</span>
            )}
            {chatStatus.broadcast_title && (
              <span className="text-ink-500">· {chatStatus.broadcast_title}</span>
            )}
            {chatStatus.last_error && (
              <span className="text-live">
                {chatStatus.last_error} {chatStatus.last_error_code && `(${chatStatus.last_error_code})`}
              </span>
            )}
          </div>
        )}

        <div className="mt-4 space-y-2">
          {messages.map((m, i) => (
            <div key={m.id} className="flex items-start gap-2">
              <span className="w-5 pt-2 text-right text-xs text-ink-500">{i + 1}</span>
              <input
                value={m.text}
                aria-label={`메시지 ${i + 1}`}
                maxLength={MAX_CHAT_TEXT}
                onChange={(e) => setMessages(messages.map((x) => x.id === m.id ? { ...x, text: e.target.value } : x))}
                onBlur={() => api.chatUpdateMessage(m.id, m.text, m.enabled).then(setMessages).catch(reportError)}
                className="flex-1 rounded-md border border-ink-700 bg-ink-900 px-3 py-2 text-sm text-ink-100 outline-none focus:border-ink-500"
              />
              <button
                type="button"
                aria-label={m.enabled ? `메시지 ${i + 1} 끄기` : `메시지 ${i + 1} 켜기`}
                onClick={() => api.chatUpdateMessage(m.id, m.text, !m.enabled).then(setMessages).catch(reportError)}
                className={`mt-1.5 rounded p-1 ${m.enabled ? 'text-ok' : 'text-ink-600'}`}
              >
                <Check size={14} />
              </button>
              <button type="button" aria-label={`메시지 ${i + 1} 위로`} onClick={() => void move(i, -1)} className="mt-1.5 rounded p-1 text-ink-500 hover:text-ink-200">
                <ChevronUp size={14} />
              </button>
              <button type="button" aria-label={`메시지 ${i + 1} 아래로`} onClick={() => void move(i, 1)} className="mt-1.5 rounded p-1 text-ink-500 hover:text-ink-200">
                <ChevronDown size={14} />
              </button>
              <button
                type="button"
                aria-label={`메시지 ${i + 1} 삭제`}
                onClick={() => api.chatDeleteMessage(m.id).then(setMessages).catch(reportError)}
                className="mt-1.5 rounded p-1 text-ink-500 hover:text-live"
              >
                <Trash2 size={14} />
              </button>
            </div>
          ))}
        </div>

        <div className="mt-3 flex gap-2">
          <Input
            value={newMessage}
            aria-label="새 메시지"
            maxLength={MAX_CHAT_TEXT}
            placeholder="🎧 지금 듣고 있는 음악이 마음에 들면 구독해주세요!"
            onChange={(e) => setNewMessage(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && newMessage.trim()) {
                api.chatAddMessage(newMessage).then((m) => { setMessages(m); setNewMessage('') }).catch(reportError)
              }
            }}
          />
          <Button
            onClick={() => api.chatAddMessage(newMessage).then((m) => { setMessages(m); setNewMessage('') }).catch(reportError)}
            disabled={!newMessage.trim()}
          >
            <span className="inline-flex items-center gap-2"><Plus size={15} /> 메시지 추가</span>
          </Button>
        </div>

        <div className="mt-4 grid gap-4 border-t border-ink-700 pt-4 md:grid-cols-2">
          <Field label="전송 순서">
            <Select
              value={chat.order}
              aria-label="전송 순서"
              onChange={(e) => void saveChat({ ...chat, order: e.target.value as ChatSettings['order'] })}
            >
              <option value="sequential">Sequential (차례대로)</option>
              <option value="random">Random (무작위)</option>
            </Select>
          </Field>
          <Field label="전송 간격" hint={`최소 ${minInterval / 60}분`}>
            <Select
              value={String(chat.interval_secs)}
              aria-label="전송 간격"
              onChange={(e) => void saveChat({ ...chat, interval_secs: Number(e.target.value) })}
            >
              {INTERVALS.filter((m) => m * 60 >= minInterval).map((m) => (
                <option key={m} value={m * 60}>{m}분</option>
              ))}
              {!INTERVALS.some((m) => m * 60 === chat.interval_secs) && (
                <option value={chat.interval_secs}>{Math.round(chat.interval_secs / 60)}분</option>
              )}
            </Select>
          </Field>
        </div>

        <Toggle
          label="방송 시작 후 첫 메시지 즉시 전송"
          checked={chat.send_on_start}
          onChange={(v) => void saveChat({ ...chat, send_on_start: v })}
        />
        <Toggle
          label="방송 종료 전 마지막 메시지 전송"
          checked={chat.send_on_end}
          onChange={(v) => void saveChat({ ...chat, send_on_end: v })}
        />
        <Toggle
          label="같은 메시지 연속 전송 금지"
          checked={chat.avoid_repeats}
          onChange={(v) => void saveChat({ ...chat, avoid_repeats: v })}
        />

        {status?.supervisor.state !== 'LIVE' && chat.enabled && (
          <p className="mt-3 flex items-center gap-2 text-xs text-ink-500">
            <Send size={12} /> 방송이 시작되면 자동으로 채팅에 연결합니다.
          </p>
        )}
      </Card>
    </div>
  )
}
