/**
 * Making and editing a broadcast: information, playlist, destination, sending,
 * schedule, and one last look.
 *
 * Sections on one page rather than a wizard of steps, because every answer is
 * short and a first-time user is better served seeing the whole shape of a
 * broadcast than being walked through it blind.
 */
import { useEffect, useMemo, useState } from "react";
import {
  Badge,
  Button,
  Card,
  Field,
  Input,
  Select,
  Toggle,
} from "@/components/ui";
import { formatDurationKo } from "@/services/format";
import { useTransport } from "../TransportContext";
import {
  DAY_NAMES,
  cctvThumbUrl,
  regionName,
  EVERY_DAY,
  PRIVACY_LABELS,
  WEEKDAYS,
  dayOn,
  emptySchedule,
  toggleDayMask,
} from "../cloud";
import type {
  BroadcastDetail,
  CctvCheck,
  CctvSource,
  CloudMedia,
  DestinationKind,
  NewItem,
  Privacy,
  Schedule,
  StreamDestination,
  VideoSource,
  YoutubeAccount,
} from "../cloud";

/** A playlist row being edited: a video plus how it should play. */
type Row = NewItem & { filename: string; duration_secs: number };

/**
 * How many cameras the select renders at once.
 *
 * One region is a country, so every US feed shares `US`: Delaware and Virginia
 * together are already past two thousand. A select that long is slow to render
 * and useless to scroll, so the list is capped and the search box is how to
 * reach what the cap leaves out. The hint under the select says how many are
 * hidden, because a silently truncated list would look like missing cameras.
 */
const CAMERA_LIST_CAP = 300;

export function BroadcastForm({
  editing,
  initialCctvUrl,
  onDone,
  onCancel,
}: {
  /** The broadcast being changed, or nothing for a new one. */
  editing?: BroadcastDetail;
  /**
   * A camera chosen in the gallery, for a new broadcast.
   *
   * Only an opening value — the form owns the field from then on, so somebody
   * who changes their mind in the picker is not overruled by where they came
   * from. It still goes through the connection test like any other address:
   * the gallery's picture was taken when the camera was last checked, which is
   * not a statement about now.
   */
  initialCctvUrl?: string;
  onDone: () => void;
  onCancel: () => void;
}) {
  const t = useTransport();
  const [media, setMedia] = useState<CloudMedia[]>([]);
  const [destinations, setDestinations] = useState<StreamDestination[]>([]);
  const [accounts, setAccounts] = useState<YoutubeAccount[]>([]);

  const [name, setName] = useState(editing?.name ?? "");
  const [title, setTitle] = useState(editing?.title ?? "");
  const [description, setDescription] = useState(editing?.description ?? "");
  const [tags, setTags] = useState(editing?.tags ?? "");
  const [category, setCategory] = useState(editing?.category ?? "");
  const [privacy, setPrivacy] = useState<Privacy>(
    editing?.privacy ?? "private",
  );
  const [destinationId, setDestinationId] = useState(
    editing?.destination_id ?? "",
  );
  // §12: which provider sends this broadcast. One being edited already has its
  // YouTube resources, so the choice is offered only when making a new one.
  const [provider, setProvider] = useState<DestinationKind>(
    editing?.youtube?.account_id ? "youtube_account" : "manual_rtmps",
  );
  const [accountId, setAccountId] = useState(
    editing?.youtube?.account_id ?? "",
  );
  const [loopAll, setLoopAll] = useState(editing?.loop_forever ?? true);
  // Where the picture comes from. `playlist` is every broadcast that existed
  // before the traffic-CCTV test, and the one the form opens on.
  const [videoSource, setVideoSource] = useState<VideoSource>(
    editing?.cctv_url || initialCctvUrl ? "traffic_cctv" : "playlist",
  );
  const [cctvUrl, setCctvUrl] = useState(editing?.cctv_url ?? initialCctvUrl ?? "");
  const [cctvCheck, setCctvCheck] = useState<CctvCheck | null>(null);
  /**
   * The URL the result in `cctvCheck` is about.
   *
   * Kept separately so that editing the address after a successful test
   * invalidates the result. Without this, testing a working camera and then
   * pasting a different address would save the second one on the first one's
   * evidence.
   */
  const [cctvCheckedUrl, setCctvCheckedUrl] = useState<string | null>(null);
  const [cctvTesting, setCctvTesting] = useState(false);
  const [cctvError, setCctvError] = useState<string | null>(null);
  const [schedule, setSchedule] = useState<Schedule>(
    editing?.schedule ?? emptySchedule(),
  );
  const [rows, setRows] = useState<Row[]>(
    (editing?.items ?? []).map((i) => ({
      media_id: i.media_id,
      enabled: i.enabled,
      repeat_count: i.repeat_count,
      filename: i.filename,
      duration_secs: i.duration_secs,
    })),
  );
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [dragging, setDragging] = useState<number | null>(null);
  /**
   * Whether this account's **paid** plan carries the CCTV source.
   *
   * Starts false so the option is never offered before the answer is known —
   * the save path refuses it anyway, and a field that appears and then is
   * rejected is worse than one that appears a moment later.
   */
  const [cctvAllowed, setCctvAllowed] = useState(false);
  /** The catalogue, loaded only when the CCTV source is actually chosen. */
  const [cameras, setCameras] = useState<CctvSource[]>([]);
  const [camerasLoaded, setCamerasLoaded] = useState(false);
  const [region, setRegion] = useState("");
  /**
   * Which authority's cameras, and a name filter.
   *
   * `region` is a country code, not a state: every US feed lands under `US`,
   * so one region now holds two thousand cameras and a single select of them
   * is not a thing anybody can choose from. The provider is what separates
   * Delaware from Virginia, and the search is what finds a road inside one.
   */
  const [cctvProvider, setCctvProvider] = useState("");
  const [cameraQuery, setCameraQuery] = useState("");
  /** Set when the chosen camera has no frame on the server to show. */
  const [thumbBroken, setThumbBroken] = useState(false);

  useEffect(() => {
    let live = true;
    Promise.all([
      t.listMedia(),
      t.listDestinations(),
      t.listYoutubeAccounts().catch(() => []),
      // An older server does not send `cctv_enabled`; a failure here must not
      // stop the rest of the form loading, so it answers "not allowed".
      t.subscription().catch(() => null),
    ])
      .then(([m, d, a, sub]) => {
        if (!live) return;
        setCctvAllowed(sub?.cctv_enabled === true);
        setMedia(m);
        setDestinations(d.filter((x) => x.kind !== "youtube_account"));
        setDestinationId(
          (current) =>
            current || d.find((x) => x.kind !== "youtube_account")?.id || "",
        );
        setAccounts(a);
        setAccountId((current) => current || a[0]?.id || "");
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [t]);

  // Loaded when the source is chosen rather than with the rest of the form:
  // most broadcasts never open this panel, and the list is a request.
  useEffect(() => {
    if (!cctvAllowed || videoSource !== "traffic_cctv" || camerasLoaded) return;
    let live = true;
    t.listCctvSources()
      .then((list) => {
        if (!live) return;
        setCameras(list);
        setCamerasLoaded(true);
        // The first region in the same order the select shows them, so the
        // default does not depend on how the server happened to order the
        // payload.
        setRegion(
          (current) =>
            current || [...new Set(list.map((c) => c.region))].sort()[0] || "",
        );
      })
      // An empty catalogue and an unreachable one look the same here on
      // purpose: either way the operator has not given this account a list to
      // pick from, and typing an address still works.
      .catch(() => {
        if (live) setCamerasLoaded(true);
      });
    return () => {
      live = false;
    };
  }, [t, cctvAllowed, videoSource, camerasLoaded]);

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
  // A provider belongs to a region, so changing the region leaves the old one
  // selecting nothing. Corrected here rather than in the region's onChange,
  // because the list also arrives after the first render.
  useEffect(() => {
    setCctvProvider((current) =>
      providers.includes(current) ? current : (providers[0] ?? ""),
    );
  }, [providers]);

  /** The catalogue row this address belongs to, when it came from the list. */
  const chosenCamera = useMemo(
    () => cameras.find((c) => c.url === cctvUrl),
    [cameras, cctvUrl],
  );

  /** What region, provider and the search box leave — the honest count. */
  const hits = useMemo(() => {
    const q = cameraQuery.trim().toLowerCase();
    const pool = cameras.filter(
      (c) => c.region === region && c.provider === cctvProvider,
    );
    return q ? pool.filter((c) => c.name.toLowerCase().includes(q)) : pool;
  }, [cameras, region, cctvProvider, cameraQuery]);

  /**
   * The options the select renders.
   *
   * Capped, because two thousand `<option>` elements are slow to render and
   * nobody scrolls that far — the search is how to reach the rest, and the
   * hint under the select says how many are hidden.
   *
   * The camera already chosen is kept in the list even when the filter would
   * drop it, so a narrowed search cannot make a made choice look lost: the
   * select would otherwise fall back to "직접 입력" over a perfectly good
   * address. It is prepended rather than counted, so it never inflates the
   * number the hint reports.
   */
  const options = useMemo(() => {
    const head = hits.slice(0, CAMERA_LIST_CAP);
    return chosenCamera && !head.some((c) => c.id === chosenCamera.id)
      ? [chosenCamera, ...head]
      : head;
  }, [hits, chosenCamera]);

  const ready = media.filter((m) => m.state === "ready");
  const unused = ready.filter((m) => !rows.some((r) => r.media_id === m.id));
  const totalSecs = useMemo(
    () =>
      rows
        .filter((r) => r.enabled)
        .reduce((n, r) => n + r.duration_secs * r.repeat_count, 0),
    [rows],
  );

  function move(from: number, to: number) {
    if (to < 0 || to >= rows.length || from === to) return;
    const next = rows.slice();
    const [row] = next.splice(from, 1);
    if (row) next.splice(to, 0, row);
    setRows(next);
  }

  function add(m: CloudMedia) {
    setRows([
      ...rows,
      {
        media_id: m.id,
        enabled: true,
        repeat_count: 1,
        filename: m.filename,
        duration_secs: m.duration_secs,
      },
    ]);
  }

  /**
   * Ask the server to open the URL.
   *
   * The server is what judges the address — it is the machine that will fetch
   * it — so nothing is validated here beyond "the user typed something". A
   * refusal arrives as an error; a reachable stream with no video arrives as a
   * result with `ok: false`.
   */
  async function testCctv() {
    const tried = cctvUrl.trim();
    setCctvTesting(true);
    setCctvError(null);
    setCctvCheck(null);
    setCctvCheckedUrl(null);
    try {
      const got = await t.testCctv(tried);
      setCctvCheck(got);
      // The address this verdict is about, not whatever is in the box by the
      // time the answer arrives.
      setCctvCheckedUrl(tried);
    } catch (e) {
      setCctvError(e instanceof Error ? e.message : "확인할 수 없습니다.");
    } finally {
      setCctvTesting(false);
    }
  }

  /** An empty string clears a stored URL; `playlist` means there is none. */
  const cctvToSave = videoSource === "traffic_cctv" ? cctvUrl.trim() : "";

  /**
   * Has this exact address been opened by the server and found to carry video?
   *
   * The reason this gates saving: the address is only checked for shape and
   * for where it points when it is stored. Whether anything readable is *at*
   * it is a different question, and the only thing that can answer it is the
   * probe. A broadcast saved with an unreadable address does not fail at save
   * time — it fails when it starts, and then the supervisor retries it every
   * sixty seconds for ever while the dashboard shows a broadcast that is
   * trying. Far better to answer the question while the field is still on
   * screen.
   */
  const cctvProven =
    videoSource !== "traffic_cctv" ||
    (cctvCheck?.ok === true && cctvCheckedUrl === cctvToSave);

  /**
   * Why saving is blocked, in the words the person needs.
   *
   * Ordered most specific first, because each of these is a different problem
   * with a different next move.
   */
  const cctvBlocker = (): string | null => {
    if (videoSource !== "traffic_cctv") return null;
    // Asked before the connection test, for the same reason the server asks the
    // plan before the address: somebody without the feature must not be sent
    // off to test a URL that was never the problem. A disabled option cannot
    // normally be chosen, so this is for the broadcast that already had a
    // camera when the plan changed under it.
    if (!cctvAllowed)
      return "CCTV 영상 송출은 Business 요금제에서만 사용할 수 있습니다.";
    if (cctvProven) return null;
    if (cctvToSave.length === 0) return "CCTV 주소를 입력해 주세요.";
    // The request itself failed, or the server refused the address outright.
    if (cctvError) return "연결 테스트가 실패했습니다. 주소를 확인해 주세요.";
    // Opened, and there is nothing usable at it.
    if (cctvCheck && cctvCheckedUrl === cctvToSave)
      return "이 주소로는 송출할 수 없습니다. 위의 실패 이유를 확인해 주세요.";
    // Tested, then edited: the evidence belongs to a different address.
    if (cctvCheckedUrl !== null)
      return "주소가 바뀌었습니다. 연결 테스트를 다시 실행해 주세요.";
    return "저장하기 전에 연결 테스트를 실행해 주세요.";
  };

  async function save() {
    setBusy(true);
    setError(null);
    try {
      const items: NewItem[] = rows.map((r) => ({
        media_id: r.media_id,
        enabled: r.enabled,
        repeat_count: Math.min(100, Math.max(1, r.repeat_count)),
      }));
      if (editing) {
        await t.updateBroadcast(editing.id, {
          name: name.trim() || "새 방송",
          title: title.trim() || name.trim(),
          description,
          tags,
          category,
          privacy,
          loop_forever: loopAll,
          // A YouTube-connected broadcast sends to the address YouTube gave it;
          // pointing it elsewhere would break the binding it already has.
          destination_id:
            provider === "manual_rtmps" ? destinationId : undefined,
          schedule,
          cctv_url: cctvToSave,
        });
        await t.replaceItems(editing.id, items);
      } else {
        await t.createBroadcast({
          name: name.trim() || "새 방송",
          // Exactly one of the two. A connected account has no destination yet:
          // YouTube makes one when the broadcast is created on its side.
          destination_id: provider === "manual_rtmps" ? destinationId : "",
          youtube_account_id:
            provider === "youtube_account" ? accountId : undefined,
          items,
          loop_forever: loopAll,
          title: title.trim() || name.trim(),
          description,
          tags,
          category,
          privacy,
          schedule,
          cctv_url: cctvToSave,
        });
      }
      onDone();
    } catch (e) {
      setError(e instanceof Error ? e.message : "저장할 수 없습니다.");
    } finally {
      setBusy(false);
    }
  }

  const destination = destinations.find((d) => d.id === destinationId);
  const account = accounts.find((a) => a.id === accountId);
  /** Is there something to send to at all? */
  const chosen =
    provider === "manual_rtmps" ? Boolean(destinationId) : Boolean(accountId);

  return (
    <div className="space-y-4" data-testid="broadcast-form">
      <div className="flex items-center justify-between">
        <h1 className="text-lg font-semibold">
          {editing ? "방송 수정" : "방송 만들기"}
        </h1>
        <div className="flex gap-2">
          <Button onClick={onCancel}>취소</Button>
          <Button
            variant="primary"
            onClick={save}
            disabled={busy || rows.length === 0 || !chosen || !cctvProven}
            title={cctvBlocker() ?? undefined}
          >
            {busy ? "저장 중…" : editing ? "저장" : "방송 만들기"}
          </Button>
        </div>
      </div>

      {error && (
        <p
          role="alert"
          className="rounded-md border border-live-dim bg-ink-850 px-4 py-3 text-sm text-live"
        >
          {error}
        </p>
      )}

      {/* 1 — what this broadcast is */}
      <Card title="1. 방송 정보">
        <Field label="이름" hint="목록에서 구분하기 위한 이름입니다.">
          <Input
            value={name}
            aria-label="이름"
            onChange={(e) => setName(e.target.value)}
            placeholder="밤 라디오"
          />
        </Field>
        <Field label="제목">
          <Input
            value={title}
            aria-label="제목"
            onChange={(e) => setTitle(e.target.value)}
            placeholder="Lofi Jazz 24/7"
          />
        </Field>
        <Field label="설명">
          <textarea
            aria-label="설명"
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={3}
            className="w-full rounded-md border border-ink-600 bg-ink-900 px-3 py-2 text-sm text-ink-100 outline-none focus:border-ink-400"
          />
        </Field>
        <div className="grid gap-3 sm:grid-cols-3">
          <Field label="태그" hint="쉼표로 구분">
            <Input
              value={tags}
              aria-label="태그"
              onChange={(e) => setTags(e.target.value)}
              placeholder="jazz, lofi"
            />
          </Field>
          <Field label="카테고리">
            <Input
              value={category}
              aria-label="카테고리"
              onChange={(e) => setCategory(e.target.value)}
              placeholder="Music"
            />
          </Field>
          <Field label="공개 범위">
            <Select
              aria-label="공개 범위"
              value={privacy}
              onChange={(e) => setPrivacy(e.target.value as Privacy)}
            >
              {(Object.keys(PRIVACY_LABELS) as Privacy[]).map((p) => (
                <option key={p} value={p}>
                  {PRIVACY_LABELS[p]}
                </option>
              ))}
            </Select>
          </Field>
        </div>
        <p className="mt-1 text-xs text-warn">
          이 정보는 247streams에 저장됩니다. 스트림 키만 등록된 대상에서는
          YouTube의 제목·설명·공개 범위를 바꾸지 않습니다 — YouTube Studio에서
          직접 설정하세요.
        </p>
      </Card>

      {/* 2 — the playlist */}
      <Card
        title="2. 영상"
        action={
          <span className="text-xs text-ink-400">
            {rows.length}개 · 한 바퀴 {formatDurationKo(totalSecs)}
          </span>
        }
      >
        {rows.length === 0 ? (
          <p className="py-4 text-center text-sm text-ink-500">
            아래에서 영상을 추가하세요.
          </p>
        ) : (
          <ul className="divide-y divide-ink-700">
            {rows.map((r, i) => (
              <li
                key={`${r.media_id}-${i}`}
                data-testid="playlist-row"
                draggable
                onDragStart={() => setDragging(i)}
                onDragOver={(e) => e.preventDefault()}
                onDrop={() => {
                  if (dragging !== null) move(dragging, i);
                  setDragging(null);
                }}
                className={`flex items-center gap-3 py-2.5 ${dragging === i ? "opacity-50" : ""}`}
              >
                <span
                  className="cursor-grab select-none text-ink-500"
                  aria-hidden
                >
                  ⠿
                </span>
                <span className="w-6 text-center font-mono text-xs text-ink-500">
                  {i + 1}
                </span>
                <span className="min-w-0 flex-1 truncate text-sm text-ink-100">
                  {r.filename}
                </span>
                <span className="font-mono text-xs text-ink-500">
                  {formatDurationKo(r.duration_secs)}
                </span>
                <label className="flex items-center gap-1 text-xs text-ink-400">
                  반복
                  <Input
                    type="number"
                    min={1}
                    max={100}
                    aria-label={`${r.filename} 반복 횟수`}
                    value={r.repeat_count}
                    // Not clamped while typing: forcing a 1 into an emptied box
                    // makes it impossible to replace the number. The clamp is
                    // applied when it is saved.
                    onChange={(e) =>
                      setRows(
                        rows.map((x, j) =>
                          j === i
                            ? {
                                ...x,
                                repeat_count: Math.min(
                                  100,
                                  Math.max(0, Number(e.target.value) || 0),
                                ),
                              }
                            : x,
                        ),
                      )
                    }
                    className="w-16"
                  />
                </label>
                <input
                  type="checkbox"
                  aria-label={`${r.filename} 사용`}
                  checked={r.enabled}
                  onChange={(e) =>
                    setRows(
                      rows.map((x, j) =>
                        j === i ? { ...x, enabled: e.target.checked } : x,
                      ),
                    )
                  }
                />
                <Button
                  size="sm"
                  aria-label={`${r.filename} 위로`}
                  onClick={() => move(i, i - 1)}
                >
                  ↑
                </Button>
                <Button
                  size="sm"
                  aria-label={`${r.filename} 아래로`}
                  onClick={() => move(i, i + 1)}
                >
                  ↓
                </Button>
                <Button
                  size="sm"
                  aria-label={`${r.filename} 제거`}
                  onClick={() => setRows(rows.filter((_, j) => j !== i))}
                >
                  ✕
                </Button>
              </li>
            ))}
          </ul>
        )}

        <div className="mt-4 border-t border-ink-700 pt-3">
          <Toggle
            checked={loopAll}
            onChange={setLoopAll}
            label="전체 반복"
            hint="끝나면 처음부터 다시 재생합니다. 24시간 방송에는 켜 두세요."
          />
        </div>

        {unused.length > 0 && (
          <div className="mt-3 border-t border-ink-700 pt-3">
            <p className="mb-2 text-xs uppercase tracking-widest text-ink-500">
              추가할 수 있는 영상
            </p>
            <div className="flex flex-wrap gap-2">
              {unused.map((m) => (
                <Button
                  key={m.id}
                  size="sm"
                  onClick={() => add(m)}
                  data-testid="add-media"
                >
                  + {m.filename}
                </Button>
              ))}
            </div>
          </div>
        )}
        {ready.length === 0 && (
          <p className="mt-3 text-xs text-warn">
            사용 가능한 영상이 없습니다. 먼저 영상 탭에서 업로드하세요.
          </p>
        )}
      </Card>

      {/* 2-1 — the traffic-CCTV test source.
          Additive on purpose: "플레이리스트 영상" is what every broadcast did
          before this existed, and it is what the form opens on. The numbering
          is 2-1 so that the sections below keep the numbers users already
          know. */}
      <Card title="2-1. 영상 소스 (테스트)">
        <Field
          label="영상"
          hint="기본은 플레이리스트의 영상입니다. Traffic CCTV를 고르면 화면은 CCTV 실시간 영상, 소리는 위 플레이리스트의 음악으로 송출합니다."
        >
          <Select
            aria-label="영상 소스"
            value={videoSource}
            onChange={(e) => {
              setVideoSource(e.target.value as VideoSource);
              setCctvCheck(null);
              setCctvError(null);
            }}
          >
            <option value="playlist">플레이리스트 영상 (기본)</option>
            {/* Disabled rather than hidden: somebody editing a broadcast that
                already has a camera must still see which source it is on, and
                somebody who does not have the feature should be able to see
                that it exists. */}
            <option value="traffic_cctv" disabled={!cctvAllowed}>
              CCTV 영상{cctvAllowed ? "" : " (Business 요금제 전용)"}
            </option>
          </Select>
        </Field>

        {!cctvAllowed && (
          <p className="mt-2 text-xs text-ink-500" data-testid="cctv-upsell">
            CCTV 영상 송출은 Business 요금제에서 사용할 수 있습니다. 무료 체험으로
            받은 권한에는 포함되지 않습니다.
          </p>
        )}

        {videoSource === "traffic_cctv" && (
          <div className="mt-3 space-y-3 rounded-md border border-ink-700 bg-ink-900 p-3">
            {cameras.length > 0 && (
              <>
                <div className="grid gap-3 sm:grid-cols-2">
                  <Field label="지역">
                    <Select
                      aria-label="CCTV 지역"
                      value={region}
                      onChange={(e) => {
                        setRegion(e.target.value);
                        // The old search was about the old region's roads.
                        setCameraQuery("");
                      }}
                    >
                      {regions.map((r) => (
                        <option key={r} value={r}>
                          {regionName(r)}
                        </option>
                      ))}
                    </Select>
                  </Field>
                  {/* Only when there is a choice to make. One authority in a
                      region does not need a select saying so. */}
                  {providers.length > 1 && (
                    <Field label="제공처">
                      <Select
                        aria-label="CCTV 제공처"
                        value={cctvProvider}
                        onChange={(e) => {
                          setCctvProvider(e.target.value);
                          setCameraQuery("");
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
                <Field
                  label="카메라 찾기"
                  hint="도로 이름이나 지역 일부를 입력하면 아래 목록이 좁혀집니다."
                >
                  <Input
                    value={cameraQuery}
                    aria-label="CCTV 카메라 검색"
                    onChange={(e) => setCameraQuery(e.target.value)}
                    placeholder="예: I-64, Arlington, Dock St"
                  />
                </Field>
                <Field
                  label="카메라"
                  hint="고르면 주소가 채워집니다. 저장하려면 연결 테스트를 한 번 더 거칩니다."
                >
                  <Select
                    aria-label="CCTV 카메라"
                    value={options.some((c) => c.url === cctvUrl) ? cctvUrl : ""}
                    onChange={(e) => {
                      setCctvUrl(e.target.value);
                      // The verdict in hand belongs to the previous address.
                      setCctvCheck(null);
                      setCctvError(null);
                      setCctvCheckedUrl(null);
                      setThumbBroken(false);
                    }}
                  >
                    <option value="">직접 입력</option>
                    {options.map((c) => (
                      <option key={c.id} value={c.url}>
                        {c.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                {/* What the list is not showing, said plainly. A capped list
                    with no note looks like cameras that were never imported. */}
                <p className="text-xs text-ink-500" data-testid="cctv-count">
                  {hits.length === 0
                    ? "검색어에 맞는 카메라가 없습니다."
                    : hits.length > CAMERA_LIST_CAP
                      ? `${hits.length}대 중 ${CAMERA_LIST_CAP}대 표시 — 검색으로 좁혀 주세요.`
                      : `${hits.length}대`}
                </p>
              </>
            )}
            {camerasLoaded && cameras.length === 0 && (
              <p className="text-xs text-ink-500" data-testid="cctv-no-catalogue">
                등록된 카메라 목록이 없습니다. 아래에 주소를 직접 입력해 주세요.
              </p>
            )}
            {/* The same frame the gallery shows, for the camera now chosen.
                Here because the picker's option text is a road name, and a
                road name does not say whether the camera points at a river or
                at the inside of a tunnel. Hidden when the address was typed
                rather than picked: there is no frame for an address this
                server has never opened. */}
            {chosenCamera && !thumbBroken && (
              <div>
                <img
                  src={cctvThumbUrl(chosenCamera.id)}
                  alt={chosenCamera.name}
                  className="aspect-video w-full max-w-sm rounded-md border border-ink-700 bg-ink-800 object-cover"
                  onError={() => setThumbBroken(true)}
                  data-testid="cctv-thumb"
                />
                <p className="mt-1 text-xs text-ink-500">
                  마지막 확인 때의 화면입니다. 실시간 영상이 아닙니다.
                </p>
              </div>
            )}
            <Field
              label="CCTV 주소"
              hint="HLS(.m3u8) 또는 FFmpeg가 바로 읽을 수 있는 http / https 라이브 주소. 내부망·localhost 주소는 서버가 거부합니다."
            >
              <Input
                value={cctvUrl}
                aria-label="CCTV 주소"
                onChange={(e) => {
                  setCctvUrl(e.target.value);
                  setCctvCheck(null);
                  setCctvError(null);
                }}
                placeholder="https://example.com/live/stream.m3u8"
              />
            </Field>
            <Button
              onClick={testCctv}
              disabled={cctvTesting || cctvUrl.trim().length === 0}
            >
              {cctvTesting ? "확인 중…" : "연결 테스트"}
            </Button>

            {cctvError && (
              <p role="alert" className="text-sm text-live">
                FAILED · {cctvError}
              </p>
            )}
            {cctvCheck && (
              <div
                role="status"
                className={`text-sm ${cctvCheck.ok ? "text-ok" : "text-warn"}`}
              >
                <p>
                  {cctvCheck.ok ? "SUCCESS" : "FAILED"} · {cctvCheck.message}
                </p>
                {cctvCheck.ok && (
                  <ul className="mt-1 space-y-0.5 font-mono text-xs text-ink-400">
                    <li>codec: {cctvCheck.video_codec ?? "—"}</li>
                    <li>
                      resolution:{" "}
                      {cctvCheck.width && cctvCheck.height
                        ? `${cctvCheck.width}×${cctvCheck.height}`
                        : "—"}
                    </li>
                    <li>fps: {cctvCheck.fps ?? "—"}</li>
                    <li>stream: {cctvCheck.stream_type ?? "—"}</li>
                  </ul>
                )}
              </div>
            )}

            {/* Why the save button is off, where the field is. A `title` only
                appears on hover, and the person who needs this sentence is
                the one wondering why nothing happens when they click. */}
            {cctvBlocker() && (
              <p role="status" className="text-xs text-warn">
                {cctvBlocker()}
              </p>
            )}

            <p className="text-xs text-ink-500">
              CCTV 원본 오디오는 송출하지 않습니다. 소리는 위 플레이리스트의
              음악만 나갑니다. 테스트 기능이므로 CCTV가 끊기면 이 방송만 재연결을
              시도합니다.
            </p>
            <p className="text-xs text-ink-500">
              CCTV 방송은 화면과 소리를 실시간으로 합쳐야 해서 일반 방송보다 서버
              자원을 훨씬 많이 씁니다. 그래서 서버가 동시에 처리할 수 있는 CCTV
              방송 수에는 요금제와 별도인 한도가 있고, 한도에 도달하면 다른 CCTV
              방송이 끝날 때까지 시작이 거절됩니다.
            </p>
            <p className="text-xs text-ink-500">
              YouTube·Twitch·치지직 등의 <strong>시청 페이지 링크는 사용할 수
              없습니다</strong>. 그 주소는 영상이 아니라 웹페이지라서 송출기가
              영상을 찾지 못합니다. CCTV·카메라가 직접 제공하는 HLS(.m3u8) 주소가
              필요합니다.
            </p>
            {rows.length === 0 && (
              <p className="text-xs text-warn">
                음악이 될 영상을 플레이리스트에 최소 한 개 넣어야 합니다.
              </p>
            )}
          </div>
        )}
      </Card>

      {/* 3 — where it goes */}
      <Card title="3. 송출 대상">
        <Field
          label="방법"
          hint="연결된 계정은 247streams가 YouTube 방송을 직접 만들고 제목과 공개 범위까지 적용합니다. 스트림 키는 영상만 보냅니다."
        >
          <Select
            aria-label="송출 방법"
            value={provider}
            disabled={Boolean(editing)}
            onChange={(e) => setProvider(e.target.value as DestinationKind)}
          >
            <option value="manual_rtmps">
              스트림 키 직접 입력 (수동 RTMPS)
            </option>
            <option value="youtube_account">연결된 YouTube 계정</option>
          </Select>
        </Field>
        {editing && (
          <p className="text-xs text-ink-500">
            만든 뒤에는 송출 방법을 바꿀 수 없습니다. 다른 방법으로 보내려면 새
            방송을 만들어 주세요.
          </p>
        )}

        {provider === "manual_rtmps" ? (
          <>
            <Field label="대상">
              <Select
                aria-label="송출 대상"
                value={destinationId}
                onChange={(e) => setDestinationId(e.target.value)}
              >
                {destinations.map((d) => (
                  <option key={d.id} value={d.id}>
                    {d.label}
                  </option>
                ))}
              </Select>
            </Field>
            {destination && (
              <p className="font-mono text-xs text-ink-500">
                {destination.rtmps_url} · {destination.key_masked}
              </p>
            )}
            {destinations.length === 0 && (
              <p className="text-xs text-warn">
                송출 대상이 없습니다. 먼저 송출 대상 탭에서 스트림 키를
                추가하세요.
              </p>
            )}
            <p className="text-xs text-ink-500">
              스트림 키는 영상만 보냅니다. 제목·설명·공개 범위는 YouTube
              Studio에서 직접 관리해야 합니다.
            </p>
          </>
        ) : (
          <>
            <Field label="채널">
              <Select
                aria-label="YouTube 채널"
                value={accountId}
                onChange={(e) => setAccountId(e.target.value)}
              >
                {accounts.map((a) => (
                  <option key={a.id} value={a.id}>
                    {a.channel_title}
                  </option>
                ))}
              </Select>
            </Field>
            {accounts.length === 0 && (
              <p className="text-xs text-warn">
                연결된 YouTube 계정이 없습니다. 먼저 송출 대상 탭에서 계정을
                연결하세요.
              </p>
            )}
            {account && (
              <p className="text-xs text-ink-500">
                방송을 만들면 {account.channel_title} 채널에 YouTube 라이브가
                생성되고, 위에서 입력한 제목과 공개 범위가 그대로 적용됩니다.
              </p>
            )}
          </>
        )}
      </Card>

      {/* 4 — how it is sent */}
      <Card title="4. 송출 설정">
        <p className="text-sm text-ink-400">
          서버가 정합니다. 고를 것은 없습니다. <Badge tone="ok">자동</Badge>
        </p>
        <ul className="mt-2 space-y-1 text-xs text-ink-500">
          <li>
            플레이리스트 방송은 올릴 때 준비된 파일을 다시 인코딩하지 않고 그대로
            보냅니다. 화질은 준비된 파일 그대로입니다.
          </li>
          <li>
            영상 소스를 쓰는 방송은 실시간으로 인코딩합니다. 화면이 1080p 보다
            크면 1080p 로 줄여 보내고, 더 작은 화면은 늘리지 않습니다.
          </li>
        </ul>
      </Card>

      {/* 5 — when */}
      <Card title="5. 예약">
        <Toggle
          checked={schedule.enabled}
          onChange={(enabled) => setSchedule({ ...schedule, enabled })}
          label="예약 시작"
          hint="브라우저를 닫아도 서버가 시간을 지켜 시작합니다."
        />
        {schedule.enabled && (
          <div className="mt-3 space-y-3">
            <div className="grid gap-3 sm:grid-cols-2">
              <Field label="시작" hint={schedule.timezone}>
                <Input
                  type="datetime-local"
                  aria-label="시작 시각"
                  value={toLocalInput(schedule.start_at)}
                  onChange={(e) =>
                    setSchedule({
                      ...schedule,
                      start_at: fromLocalInput(e.target.value),
                    })
                  }
                />
              </Field>
              <Field label="종료 (선택)">
                <Input
                  type="datetime-local"
                  aria-label="종료 시각"
                  value={toLocalInput(schedule.stop_at)}
                  onChange={(e) =>
                    setSchedule({
                      ...schedule,
                      stop_at: fromLocalInput(e.target.value),
                    })
                  }
                />
              </Field>
            </div>
            <div>
              <p className="mb-2 text-sm text-ink-100">반복</p>
              <div className="flex flex-wrap gap-2">
                <Button
                  size="sm"
                  onClick={() => setSchedule({ ...schedule, repeat_days: 0 })}
                >
                  한 번만
                </Button>
                <Button
                  size="sm"
                  onClick={() =>
                    setSchedule({ ...schedule, repeat_days: EVERY_DAY })
                  }
                >
                  매일
                </Button>
                <Button
                  size="sm"
                  onClick={() =>
                    setSchedule({ ...schedule, repeat_days: WEEKDAYS })
                  }
                >
                  주중
                </Button>
                {DAY_NAMES.map((d, i) => (
                  <Button
                    key={d}
                    size="sm"
                    aria-label={`${d}요일`}
                    aria-pressed={dayOn(schedule.repeat_days, i)}
                    variant={
                      dayOn(schedule.repeat_days, i) ? "primary" : "ghost"
                    }
                    onClick={() =>
                      setSchedule({
                        ...schedule,
                        repeat_days: toggleDayMask(schedule.repeat_days, i),
                      })
                    }
                  >
                    {d}
                  </Button>
                ))}
              </div>
            </div>
          </div>
        )}
      </Card>

      {/* 6 — the summary, so nothing is a surprise */}
      <Card title="6. 확인">
        <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-2">
          <Row label="이름" value={name || "새 방송"} />
          <Row label="공개 범위" value={PRIVACY_LABELS[privacy]} />
          <Row
            label="영상"
            value={`${rows.filter((r) => r.enabled).length}개 · ${formatDurationKo(totalSecs)}`}
          />
          <Row label="전체 반복" value={loopAll ? "켜짐" : "한 바퀴만"} />
          <Row
            label="대상"
            value={
              provider === "youtube_account"
                ? `YouTube 계정 · ${account?.channel_title ?? "—"}`
                : `수동 RTMPS · ${destination?.label ?? "—"}`
            }
          />
          <Row
            label="송출 방식"
            value={
              videoSource === "traffic_cctv"
                ? "영상 소스 실시간 인코딩 · 최대 1080p"
                : "준비된 파일 그대로 복사"
            }
          />
        </dl>
        <p className="mt-3 text-xs text-ink-500">
          만든 뒤 대시보드에서 시작합니다. 시작하면 브라우저를 닫아도 서버에서
          계속 송출됩니다.
        </p>
      </Card>
    </div>
  );
}

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-4 border-b border-ink-800 py-1">
      <dt className="text-ink-500">{label}</dt>
      <dd className="text-ink-100">{value}</dd>
    </div>
  );
}

/** UTC in the row, the reader's own clock in the box. */
function toLocalInput(utc?: string | null): string {
  if (!utc) return "";
  const d = new Date(utc);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

function fromLocalInput(local: string): string | null {
  if (!local) return null;
  const d = new Date(local);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}
