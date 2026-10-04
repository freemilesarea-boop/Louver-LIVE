/**
 * Sign in, or make an account. Nothing else is reachable until this succeeds.
 *
 * One card, two modes. Signing in stays four lines long, because somebody
 * returning to a running broadcast should not have to read anything; signing up
 * asks for a name, a password twice, and agreement to the terms, because those
 * are the things that cannot be collected later without interrupting them.
 *
 * Every check below is also made by the server, which is the one that decides.
 * These exist to answer without a round trip, and to keep a mistyped password
 * from becoming a request at all.
 */
import { useState } from "react";
import { Button, Card, Field, Input } from "@/components/ui";
import { Wordmark } from "../App";
import { useTransport } from "../TransportContext";
import { MAX_NAME_CHARS, MIN_PASSWORD_CHARS } from "../cloud";
import type { Me } from "../cloud";

type Mode = "login" | "register";

/**
 * `heading` is false where the page already has an `<h1>` — the public landing
 * page embeds this card under its own heading, and a second `<h1>` on one page
 * is a worse answer to "what is this page about" than none.
 */
export function SignIn({
  onSignedIn,
  heading = true,
}: {
  onSignedIn: (me: Me) => void;
  heading?: boolean;
}) {
  const t = useTransport();
  const [mode, setMode] = useState<Mode>("login");
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirmation, setConfirmation] = useState("");
  const [agreed, setAgreed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /**
   * Why the form cannot be sent yet, or `null` when it can.
   *
   * Returned rather than thrown so that the same function can decide both the
   * message and whether to make the request. The order matters: it is the order
   * the fields are in, so the error always points at the first thing to fix
   * rather than the last thing checked.
   */
  function whatIsWrong(): string | null {
    if (mode === "login") {
      if (email.trim().length === 0) return "이메일을 입력해주세요.";
      if (password.length === 0) return "비밀번호를 입력해주세요.";
      // Nothing else: whether the pair is right is the server's answer, and the
      // same one for a wrong password as for an unknown address.
      return null;
    }
    if (name.trim().length === 0) return "이름을 입력해주세요.";
    if (name.trim().length > MAX_NAME_CHARS)
      return `이름은 ${MAX_NAME_CHARS}자 이내로 입력해주세요.`;
    if (!email.includes("@")) return "올바른 이메일 주소를 입력해주세요.";
    if (password.length < MIN_PASSWORD_CHARS)
      return `비밀번호는 ${MIN_PASSWORD_CHARS}자 이상이어야 합니다.`;
    if (password !== confirmation) return "비밀번호가 일치하지 않습니다.";
    if (!agreed) return "이용약관 및 개인정보처리방침에 동의해주세요.";
    return null;
  }

  /** Leaving a mode must not leave its fields behind to be submitted later. */
  function switchTo(next: Mode) {
    setMode(next);
    setError(null);
    setPassword("");
    setConfirmation("");
    setAgreed(false);
    if (next === "login") setName("");
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (busy) return; // a second Enter while the first is in flight

    const wrong = whatIsWrong();
    if (wrong) {
      // Deliberately no request: a mistyped confirmation is not the server's
      // problem to answer.
      setError(wrong);
      return;
    }

    setBusy(true);
    setError(null);
    try {
      const me =
        mode === "login"
          ? await t.login(email, password)
          : await t.register(name.trim(), email.trim(), password);
      // Neither password is held beyond this call, and there is no token to put
      // anywhere: the server sets an HttpOnly cookie.
      setPassword("");
      setConfirmation("");
      onSignedIn(me);
    } catch (e) {
      // The server's own message when it has one — it says "이미 가입된
      // 이메일입니다" better than this file could guess. The name and the email
      // are left as typed, because retyping them is the last thing somebody
      // wants after a failure; the passwords are cleared.
      setError(e instanceof Error ? e.message : fallbackError(mode));
      setPassword("");
      setConfirmation("");
    } finally {
      setBusy(false);
    }
  }

  const registering = mode === "register";

  return (
    <div
      className={
        heading
          ? "flex min-h-screen items-center justify-center bg-ink-950 p-6"
          : "flex justify-center"
      }
    >
      <div className="w-full max-w-sm">
        {heading && (
          <h1 className="mb-6 text-center text-2xl">
            <Wordmark />
          </h1>
        )}
        <Card title={registering ? "247streams 시작하기" : "로그인"}>
          {registering && (
            <p className="-mt-1 pb-1 text-sm text-ink-400">
              24시간 YouTube 라이브를 클라우드에서 운영하세요.
            </p>
          )}
          {/*
            `noValidate` so that the messages below are the ones shown.
            `type="email"` still earns its keep — it is what gets the right
            keyboard on a phone — but the browser's own bubble for it is in the
            browser's language, appears before this component runs, and would
            make "올바른 이메일 주소를 입력해주세요." unreachable. One set of
            messages, in one language, in one order.
          */}
          <form onSubmit={submit} noValidate>
            {registering && (
              <Field label="이름">
                <Input
                  aria-label="이름"
                  autoComplete="name"
                  placeholder="홍길동"
                  maxLength={MAX_NAME_CHARS}
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  required
                />
              </Field>
            )}
            <Field label="이메일">
              <Input
                type="email"
                aria-label="이메일"
                autoComplete="email"
                placeholder="name@example.com"
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                required
              />
            </Field>
            <Field
              label="비밀번호"
              hint={registering ? `${MIN_PASSWORD_CHARS}자 이상` : undefined}
            >
              <Input
                type="password"
                aria-label="비밀번호"
                autoComplete={registering ? "new-password" : "current-password"}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                required
              />
            </Field>
            {registering && (
              <Field label="비밀번호 확인">
                <Input
                  type="password"
                  aria-label="비밀번호 확인"
                  autoComplete="new-password"
                  value={confirmation}
                  onChange={(e) => setConfirmation(e.target.value)}
                  required
                />
              </Field>
            )}

            {registering && (
              <label className="mt-2 flex items-start gap-2 text-sm text-ink-100">
                <input
                  type="checkbox"
                  aria-label="이용약관 및 개인정보처리방침에 동의합니다."
                  checked={agreed}
                  onChange={(e) => setAgreed(e.target.checked)}
                  className="mt-0.5 h-4 w-4 shrink-0 rounded border-ink-600 bg-ink-900 accent-ok"
                />
                <span>
                  <a
                    href="/terms"
                    target="_blank"
                    rel="noreferrer"
                    className="underline hover:text-ok"
                  >
                    이용약관
                  </a>{" "}
                  및{" "}
                  <a
                    href="/privacy"
                    target="_blank"
                    rel="noreferrer"
                    className="underline hover:text-ok"
                  >
                    개인정보처리방침
                  </a>
                  에 동의합니다. <span className="text-ink-400">(필수)</span>
                </span>
              </label>
            )}

            {error && (
              <p role="alert" className="py-2 text-sm text-live">
                {error}
              </p>
            )}
            <Button
              type="submit"
              variant="primary"
              className="mt-3 w-full"
              disabled={busy}
            >
              {busy
                ? registering
                  ? "계정 만드는 중…"
                  : "확인 중…"
                : registering
                  ? "무료로 시작하기"
                  : "로그인"}
            </Button>
          </form>
          <button
            type="button"
            className="mt-4 w-full text-xs text-ink-400 hover:text-ink-100"
            onClick={() => switchTo(registering ? "login" : "register")}
          >
            {registering
              ? "이미 계정이 있으신가요? 로그인"
              : "계정이 없으신가요? 회원가입"}
          </button>
        </Card>
      </div>
    </div>
  );
}

/** When the failure carried no message of its own — a dropped connection, say. */
function fallbackError(mode: Mode): string {
  return mode === "register"
    ? "계정을 만들지 못했습니다. 잠시 후 다시 시도해주세요."
    : "로그인에 실패했습니다. 잠시 후 다시 시도해주세요.";
}
