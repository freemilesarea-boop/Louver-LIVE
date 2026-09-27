/**
 * Connected YouTube channels. §3, §12.
 *
 * A connected channel is what lets 247streams create the live broadcast, title
 * it and set its privacy. A pasted stream key can only send video — the two live
 * side by side on purpose, and the difference is said out loud here rather than
 * left for a user to discover when their title does not change.
 *
 * No token ever reaches this component, because the API has no field that could
 * carry one.
 */
import { useCallback, useEffect, useState } from "react";
import { Button, Card, EmptyState } from "@/components/ui";
import { useTransport } from "../TransportContext";
import type { YoutubeAccount, YoutubeAvailability } from "../cloud";

export function YoutubeAccounts() {
  const t = useTransport();
  const [accounts, setAccounts] = useState<YoutubeAccount[]>([]);
  const [available, setAvailable] = useState<YoutubeAvailability | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [list, availability] = await Promise.all([
        t.listYoutubeAccounts(),
        t.youtubeAvailability(),
      ]);
      setAccounts(list);
      setAvailable(availability);
    } catch {
      /* the next action that fails will say so */
    }
  }, [t]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  async function connect() {
    setBusy(true);
    setError(null);
    try {
      // The server builds the URL, because it is the only side that can write
      // the single-use state the callback will check.
      window.location.assign(await t.youtubeConsentUrl());
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "YouTube 연결을 시작할 수 없습니다.",
      );
    } finally {
      setBusy(false);
    }
  }

  const configured = available?.configured ?? false;

  return (
    <Card
      title="YouTube 계정"
      action={
        configured ? (
          <Button variant="primary" size="sm" onClick={connect} disabled={busy}>
            YouTube 계정 연결
          </Button>
        ) : undefined
      }
    >
      {error && (
        <p role="alert" className="mb-3 text-sm text-live">
          {error}
        </p>
      )}

      {!configured ? (
        <EmptyState
          title="이 서버에는 YouTube 연결이 준비되지 않았습니다"
          hint="관리자가 YOUTUBE_CLIENT_ID 와 YOUTUBE_CLIENT_SECRET 을 설정하면 계정을 연결할 수 있습니다. 그때까지는 스트림 키를 직접 넣어 송출할 수 있습니다."
        />
      ) : accounts.length === 0 ? (
        <EmptyState
          title="연결된 채널이 없습니다"
          hint="계정을 연결하면 247streams가 방송을 직접 만들고 제목과 공개 범위까지 YouTube에 적용합니다."
        />
      ) : (
        <ul className="divide-y divide-ink-700">
          {accounts.map((a) => (
            <li
              key={a.id}
              className="flex items-center justify-between gap-4 py-3"
              data-testid="youtube-account-row"
            >
              <div className="flex min-w-0 items-center gap-3">
                {a.thumbnail_url && (
                  <img
                    src={a.thumbnail_url}
                    alt=""
                    className="h-8 w-8 rounded-full"
                  />
                )}
                <div className="min-w-0">
                  <div className="truncate text-sm text-ink-100">
                    {a.channel_title}
                  </div>
                  <div className="mt-0.5 font-mono text-xs text-ink-500">
                    {a.channel_id}
                  </div>
                </div>
                <span className="rounded-full bg-ok/10 px-2 py-0.5 text-xs text-ok">
                  연결됨
                </span>
              </div>
              <Button
                size="sm"
                onClick={async () => {
                  setError(null);
                  try {
                    await t.disconnectYoutubeAccount(a.id);
                    await refresh();
                  } catch (e) {
                    setError(
                      e instanceof Error
                        ? e.message
                        : "연결을 해제할 수 없습니다.",
                    );
                  }
                }}
              >
                연결 해제
              </Button>
            </li>
          ))}
        </ul>
      )}

      {configured && available?.redirect_uri && (
        <p className="mt-3 text-xs text-ink-500">
          Google Cloud 콘솔에 등록해야 하는 리디렉션 URI:{" "}
          <span className="font-mono text-ink-400">
            {available.redirect_uri}
          </span>
        </p>
      )}
    </Card>
  );
}
