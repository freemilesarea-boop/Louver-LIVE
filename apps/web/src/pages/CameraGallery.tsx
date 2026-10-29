/**
 * Browsing the camera catalogue by what it looks like.
 *
 * The picker in the broadcast form answers "which camera", and it answers it
 * with names — `[경부선] [부산]경부동탄터널(부산1)` is a working camera and a
 * tunnel wall, and the name is the only way to tell before going on air. This
 * page exists because somebody paying for the feature should be able to see
 * the picture first.
 *
 * The pictures are frames the server grabbed when it last checked each camera,
 * not live video. One still per card rather than a live player, because a grid
 * of fifty live HLS streams is fifty connections out of the viewer's browser
 * to somebody else's traffic authority, and the answer to "is this a river or
 * a tunnel" does not need thirty frames a second.
 */
import { useEffect, useMemo, useState } from "react";
import { Button, Card, Field, Input, Select } from "@/components/ui";
import { useTransport } from "../TransportContext";
import { cctvThumbUrl, regionName } from "../cloud";
import type { CctvSource } from "../cloud";

/**
 * How many cards one screenful adds.
 *
 * Each card is a request for an image, so this is a limit on what the server
 * is asked for at once, not just on what the page renders. Two thousand cards
 * would be two thousand frames fetched because somebody opened a tab.
 */
const PAGE = 48;

export function CameraGallery({ onUse }: { onUse?: (url: string) => void }) {
  const t = useTransport();
  const [allowed, setAllowed] = useState<boolean | null>(null);
  const [cameras, setCameras] = useState<CctvSource[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [region, setRegion] = useState("");
  const [provider, setProvider] = useState("");
  const [query, setQuery] = useState("");
  const [shown, setShown] = useState(PAGE);
  /** Cameras whose frame could not be fetched; the card shows a note instead. */
  const [broken, setBroken] = useState<Record<string, true>>({});

  // The plan is asked first and the catalogue only afterwards: for an account
  // without the feature the list answers 402, and an error banner is a worse
  // way to say "this is a Business feature" than simply saying it.
  useEffect(() => {
    let live = true;
    t.subscription()
      .then((s) => {
        if (!live) return;
        setAllowed(s?.cctv_enabled === true);
      })
      .catch(() => {
        if (live) setAllowed(false);
      });
    return () => {
      live = false;
    };
  }, [t]);

  useEffect(() => {
    if (allowed !== true) return;
    let live = true;
    t.listCctvSources()
      .then((list) => {
        if (!live) return;
        setCameras(list);
        setLoaded(true);
        setRegion(
          (current) =>
            current || [...new Set(list.map((c) => c.region))].sort()[0] || "",
        );
      })
      .catch((e: unknown) => {
        if (!live) return;
        setLoaded(true);
        setError(e instanceof Error ? e.message : "목록을 불러올 수 없습니다.");
      });
    return () => {
      live = false;
    };
  }, [t, allowed]);

  const regions = useMemo(
    () => [...new Set(cameras.map((c) => c.region))].sort(),
    [cameras],
  );
  const providers = useMemo(
    () =>
      [
        ...new Set(cameras.filter((c) => c.region === region).map((c) => c.provider)),
      ].sort(),
    [cameras, region],
  );
  // A provider belongs to a region, so a region change leaves the old one
  // selecting nothing.
  useEffect(() => {
    setProvider((current) =>
      providers.includes(current) ? current : (providers[0] ?? ""),
    );
  }, [providers]);

  const hits = useMemo(() => {
    const q = query.trim().toLowerCase();
    const pool = cameras.filter(
      (c) => c.region === region && c.provider === provider,
    );
    return q ? pool.filter((c) => c.name.toLowerCase().includes(q)) : pool;
  }, [cameras, region, provider, query]);

  // Back to the first screenful whenever the set changes, or "더 보기" would
  // keep a count that belongs to a filter nobody is looking at any more.
  useEffect(() => {
    setShown(PAGE);
  }, [region, provider, query]);

  if (allowed === null) {
    return <Card title="CCTV 둘러보기">불러오는 중…</Card>;
  }
  if (!allowed) {
    return (
      <Card title="CCTV 둘러보기">
        <p className="text-sm text-ink-300" data-testid="gallery-upsell">
          CCTV 영상 송출은 Business 요금제에서 사용할 수 있습니다. 무료 체험으로
          받은 권한에는 포함되지 않습니다.
        </p>
      </Card>
    );
  }

  return (
    <div className="space-y-4">
      <Card title="CCTV 둘러보기">
        <p className="text-sm text-ink-300">
          서버가 마지막으로 확인했을 때의 화면입니다. 실시간 영상이 아니라
          한 장면이고, 방송을 시작하면 실시간으로 나갑니다.
        </p>
        {error && (
          <p role="alert" className="mt-2 text-sm text-live">
            {error}
          </p>
        )}
        {loaded && !error && cameras.length === 0 && (
          <p className="mt-2 text-sm text-warn" data-testid="gallery-empty">
            등록된 카메라가 없습니다.
          </p>
        )}
        {cameras.length > 0 && (
          <div className="mt-3 space-y-3">
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="지역">
                <Select
                  aria-label="지역"
                  value={region}
                  onChange={(e) => {
                    setRegion(e.target.value);
                    setQuery("");
                  }}
                >
                  {regions.map((r) => (
                    <option key={r} value={r}>
                      {regionName(r)}
                    </option>
                  ))}
                </Select>
              </Field>
              {providers.length > 1 && (
                <Field label="제공처">
                  <Select
                    aria-label="제공처"
                    value={provider}
                    onChange={(e) => {
                      setProvider(e.target.value);
                      setQuery("");
                    }}
                  >
                    {providers.map((p) => (
                      <option key={p} value={p}>
                        {p}
                      </option>
                    ))}
                  </Select>
                </Field>
              )}
            </div>
            <Field label="카메라 찾기" hint="도로 이름이나 지역 일부를 입력하세요.">
              <Input
                aria-label="카메라 찾기"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="예: I-64, Arlington, 판교"
              />
            </Field>
            <p className="text-xs text-ink-500" data-testid="gallery-count">
              {hits.length === 0
                ? "검색어에 맞는 카메라가 없습니다."
                : `${hits.length}대 중 ${Math.min(shown, hits.length)}대 표시`}
            </p>
          </div>
        )}
      </Card>

      {hits.length > 0 && (
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
          {hits.slice(0, shown).map((c) => (
            <div
              key={c.id}
              className="overflow-hidden rounded-lg border border-ink-700 bg-ink-900"
              data-testid="camera-card"
            >
              {broken[c.id] ? (
                <div className="flex aspect-video items-center justify-center bg-ink-800 text-xs text-ink-500">
                  미리보기 없음
                </div>
              ) : (
                <img
                  src={cctvThumbUrl(c.id)}
                  alt={c.name}
                  // Lazy, so opening the tab fetches what is on screen rather
                  // than every card at once.
                  loading="lazy"
                  className="aspect-video w-full bg-ink-800 object-cover"
                  onError={() => setBroken((b) => ({ ...b, [c.id]: true }))}
                />
              )}
              <div className="space-y-2 p-3">
                <p className="text-sm text-ink-100">{c.name}</p>
                <p className="text-xs text-ink-500">{c.provider}</p>
                {onUse && (
                  <Button onClick={() => onUse(c.url)} data-testid="use-camera">
                    이 카메라로 방송 만들기
                  </Button>
                )}
              </div>
            </div>
          ))}
        </div>
      )}

      {hits.length > shown && (
        <Button onClick={() => setShown((n) => n + PAGE)} data-testid="gallery-more">
          더 보기 ({hits.length - shown}대 남음)
        </Button>
      )}
    </div>
  );
}
