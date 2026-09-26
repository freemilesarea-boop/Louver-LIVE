/** Sign in, or make an account. Nothing else is reachable until this succeeds. */
import { useState } from 'react'
import { Button, Card, Field, Input } from '@/components/ui'
import { useTransport } from '../TransportContext'
import type { Me } from '../cloud'

export function SignIn({ onSignedIn }: { onSignedIn: (me: Me) => void }) {
  const t = useTransport()
  const [mode, setMode] = useState<'login' | 'register'>('login')
  const [email, setEmail] = useState('')
  const [password, setPassword] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError(null)
    try {
      const me = mode === 'login' ? await t.login(email, password) : await t.register(email, password)
      // The password is never held beyond this call, and there is no token to
      // put anywhere: the server sets an HttpOnly cookie.
      setPassword('')
      onSignedIn(me)
    } catch (e) {
      setError(e instanceof Error ? e.message : '로그인에 실패했습니다.')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div className="flex min-h-screen items-center justify-center bg-ink-950 p-6">
      <div className="w-full max-w-sm">
        <h1 className="mb-6 text-center text-lg font-semibold text-ink-100">Louver Live</h1>
        <Card title={mode === 'login' ? '로그인' : '계정 만들기'}>
          <form onSubmit={submit}>
            <Field label="이메일">
              <Input
                type="email"
                autoComplete="email"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                required
              />
            </Field>
            <Field label="비밀번호" hint={mode === 'register' ? '10자 이상' : undefined}>
              <Input
                type="password"
                autoComplete={mode === 'login' ? 'current-password' : 'new-password'}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                required
              />
            </Field>
            {error && (
              <p role="alert" className="py-2 text-sm text-live">
                {error}
              </p>
            )}
            <Button type="submit" variant="primary" className="mt-3 w-full" disabled={busy}>
              {busy ? '확인 중…' : mode === 'login' ? '로그인' : '계정 만들기'}
            </Button>
          </form>
          <button
            type="button"
            className="mt-4 w-full text-xs text-ink-400 hover:text-ink-100"
            onClick={() => {
              setMode(mode === 'login' ? 'register' : 'login')
              setError(null)
            }}
          >
            {mode === 'login' ? '계정이 없으신가요? 만들기' : '이미 계정이 있으신가요? 로그인'}
          </button>
        </Card>
      </div>
    </div>
  )
}
