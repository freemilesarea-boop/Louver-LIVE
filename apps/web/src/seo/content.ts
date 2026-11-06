/**
 * Everything the public, crawlable pages say — in one module, as data.
 *
 * Two different programs render this: the React app (a visitor who arrives with
 * JavaScript) and `scripts/seo-prerender.mjs` (a crawler that fetches HTML and
 * may never run a script). They must say the same thing, so neither of them
 * owns any copy. Both read this file.
 *
 * Rules this file is held to, enforced by `seo.test.tsx`:
 *  - Every claim here must be something the product actually does. No
 *    "never drops", no "100% uptime", no promises about views or revenue.
 *  - Prices and limits must equal `SEED_PLANS` in `crates/louver-cloud/src/db.rs`,
 *    which is the one place the server reads them from. The test parses that
 *    file and compares, so a price change there fails the build here.
 *  - No invented social proof: no ratings, no reviews, no customer counts.
 */

/** The production origin. Every canonical and sitemap URL is built from it. */
export const SITE_ORIGIN = "https://247streams.kr";
export const SITE_NAME = "247streams";

/**
 * How a public URL is spelled.
 *
 * The server serves the built files with `ServeDir`, which answers
 * `/youtube-24-live/` from `youtube-24-live/index.html` and redirects
 * `/youtube-24-live` to it. So the trailing slash is the real URL, and every
 * canonical, sitemap entry and internal link below uses that form — a crawler
 * following our own links or our sitemap never meets the redirect.
 */
export function canonicalFor(path: string): string {
  return `${SITE_ORIGIN}${path}`;
}

export interface Faq {
  q: string;
  a: string;
}

export interface Section {
  heading: string;
  /** Paragraphs, in order. */
  body: string[];
  /** An optional list under the paragraphs. */
  points?: string[];
}

export interface PublicPage {
  /** The URL path, with its trailing slash — except the root, which is "/". */
  path: string;
  /** `<title>`. Unique across pages. */
  title: string;
  /** `<meta name="description">`. Unique across pages. */
  description: string;
  /** The one `<h1>` on the page. */
  h1: string;
  /** The sentence under the h1. */
  lead: string;
  sections: Section[];
  faq: Faq[];
  /** Where this page points next, by path. Rendered as real links. */
  related: string[];
  /** Included in the sitemap, and indexable. False keeps it out of both. */
  indexable: boolean;
}

/* ---------------------------------------------------------------------- *\
   The plans.

   Copied from `SEED_PLANS` on purpose, and checked against it by a test: the
   prerendered HTML is written at build time, when there is no server and no
   database to ask. The test is what keeps the copy honest.
\* ---------------------------------------------------------------------- */

export interface PublicPlan {
  id: "basic" | "pro" | "business";
  label: string;
  monthlyKrw: number;
  /** Simultaneous live streams. `max_concurrent_streams`. */
  concurrent: number;
  /** Total storage, in whole GiB. `max_storage_bytes`. */
  storageGb: number;
  /** Largest single upload, in whole GiB. `max_upload_bytes`. */
  uploadGb: number;
  /** Saved broadcasts. `max_broadcasts`. */
  broadcasts: number;
  who: string;
}

export const PUBLIC_PLANS: PublicPlan[] = [
  {
    id: "basic",
    label: "Basic",
    monthlyKrw: 19_900,
    concurrent: 1,
    storageGb: 15,
    uploadGb: 10,
    broadcasts: 3,
    who: "채널 하나를 24시간 돌리는 경우",
  },
  {
    id: "pro",
    label: "Pro",
    monthlyKrw: 39_900,
    concurrent: 2,
    storageGb: 30,
    uploadGb: 15,
    broadcasts: 10,
    who: "두 개 채널을 동시에 운영하는 경우",
  },
  {
    id: "business",
    label: "Business",
    monthlyKrw: 59_900,
    concurrent: 3,
    storageGb: 60,
    uploadGb: 20,
    broadcasts: 30,
    who: "세 개 채널과 여러 플레이리스트를 함께 운영하는 경우",
  },
];

/** What every plan includes. Each line is a feature that exists today. */
export const PLAN_INCLUDES = [
  "24시간 연속 송출",
  "YouTube 계정 연결 (OAuth)",
  "여러 영상을 이어 붙이는 플레이리스트 송출",
  "시작 시각 예약",
  "송출이 끊기면 자동으로 다시 연결",
  "송출 기록 (시작·종료 시각, 전송량, 오류)",
] as const;

export function formatKrw(won: number): string {
  return `₩${won.toLocaleString("ko-KR")}`;
}

/* ---------------------------------------------------------------------- *\
   The pages.
\* ---------------------------------------------------------------------- */

const HOME: PublicPage = {
  path: "/",
  title: "247streams | 유튜브 24시간 라이브 자동 송출",
  description:
    "유튜브 플레이리스트 · 24시간 라이브 자동 송출 서비스. PC나 OBS를 계속 켜두지 않아도 서버에서 YouTube 라이브를 자동으로 송출합니다.",
  // The first words a search result shows, and the first words on the page.
  // They have to answer "what is this service" on their own: Google picks the
  // snippet from the body when it likes the body better than the description,
  // and an opening sentence about where the software runs told a first-time
  // visitor nothing about what it does.
  h1: "유튜브 플레이리스트 · 24시간 라이브 자동 송출",
  lead: "PC를 계속 켜두지 않아도 됩니다. 영상과 플레이리스트를 등록하면 247streams가 서버에서 YouTube 라이브를 24시간 자동 송출합니다.",
  sections: [
    {
      heading: "어떻게 동작하나요",
      body: [
        "247streams는 업로드한 영상 파일을 방송에 쓸 수 있는 형식으로 서버에서 변환한 뒤, 그 파일을 YouTube의 라이브 스트림으로 내보냅니다. 송출을 실행하는 주체가 이용자의 컴퓨터가 아니라 서버라는 점이 OBS로 직접 방송하는 방식과의 차이입니다.",
      ],
      points: [
        "영상 업로드 — 서버가 방송용 규격으로 변환합니다",
        "YouTube 계정 연결 — 구글 로그인으로 권한을 위임합니다. 스트림 키를 직접 입력하는 방식도 지원합니다",
        "방송 구성 — 영상 하나 또는 플레이리스트, 반복 여부, 시작 시각을 정합니다",
        "송출 시작 — 이후의 송출과 재연결은 서버에서 처리됩니다",
      ],
    },
    {
      heading: "서버에서 송출하면 달라지는 것",
      body: [
        "24시간 방송을 개인 PC로 유지하려면 그 PC가 24시간 켜져 있어야 합니다. 절전, 윈도우 업데이트 재시작, 공유기 재부팅, 외출 중 정전이 모두 방송 중단으로 이어집니다.",
        "247streams는 송출 과정을 서버로 옮깁니다. 연결이 끊어지면 서버가 자동으로 다시 연결을 시도하고, 그 기록을 방송 이력에 남깁니다. 다만 서버와 YouTube 양쪽 모두 장애가 있을 수 있는 시스템이며, 중단이 전혀 일어나지 않는다고 보장하지는 않습니다.",
      ],
    },
    {
      heading: "어떤 채널이 쓰고 있나요",
      body: [
        "음악·앰비언스 채널처럼 같은 영상을 길게 반복 재생하는 방송, 여러 트랙을 플레이리스트로 이어 붙이는 방송, 정해진 시각에 시작해야 하는 방송에 맞게 만들어졌습니다.",
      ],
    },
  ],
  faq: [
    {
      q: "내 컴퓨터를 꺼도 방송이 계속되나요?",
      a: "네. 송출은 247streams 서버에서 실행되므로, 방송을 시작한 뒤에는 이용자의 컴퓨터나 인터넷 연결 상태와 무관하게 유지됩니다.",
    },
    {
      q: "OBS나 별도 프로그램을 설치해야 하나요?",
      a: "웹에서 사용하는 경우 설치할 것은 없습니다. 브라우저에서 영상을 업로드하고 방송을 구성하면 됩니다.",
    },
    {
      q: "어떤 영상 파일을 올릴 수 있나요?",
      a: "일반적인 동영상 파일을 업로드하면 서버가 방송용 형식으로 변환합니다. 파일 하나의 최대 크기는 요금제에 따라 10GB에서 20GB입니다.",
    },
    {
      q: "무료로 써볼 수 있나요?",
      a: "현재는 무료 요금제가 없습니다. 월 구독 요금제 세 가지를 제공하며, 가장 낮은 요금제는 월 19,900원입니다.",
    },
  ],
  related: ["/youtube-24-live/", "/playlist-live/", "/pricing/"],
  indexable: true,
};

const YOUTUBE_24: PublicPage = {
  path: "/youtube-24-live/",
  title: "유튜브 24시간 라이브 방송 켜두는 방법 | 247streams",
  description:
    "컴퓨터와 OBS를 계속 켜두지 않고 유튜브 24시간 라이브를 유지하는 방법. 업로드한 영상을 클라우드 서버가 반복 송출하고, 연결이 끊기면 자동으로 다시 연결합니다.",
  h1: "유튜브 24시간 라이브 방송, 컴퓨터를 켜두지 않고 유지하기",
  lead: "24시간 라이브의 가장 큰 문제는 영상이 아니라 '송출을 계속 돌리는 컴퓨터'입니다. 247streams는 그 역할을 서버가 대신합니다.",
  sections: [
    {
      heading: "개인 PC로 24시간 방송할 때 생기는 일",
      body: [
        "24시간 라이브를 집에서 유지하려면 PC와 인터넷이 하루도 쉬지 않아야 합니다. 실제로 방송이 끊기는 원인은 대부분 영상 문제가 아니라 환경 문제입니다.",
      ],
      points: [
        "절전 모드 진입, 자동 업데이트 후 재시작",
        "공유기 재부팅이나 회선 순단",
        "외출·수면 중에 일어난 중단을 몇 시간 뒤에 알게 되는 것",
        "PC를 계속 켜두는 데 드는 전기 요금과 발열",
      ],
    },
    {
      heading: "247streams가 처리하는 방식",
      body: [
        "영상 파일을 한 번 업로드하면 서버에 보관되고, 방송은 서버에서 시작됩니다. 송출 중 연결이 끊어지면 서버가 자동으로 다시 연결을 시도하며, 재연결 횟수와 시각이 방송 기록에 남습니다.",
        "방송을 시작한 뒤에는 브라우저를 닫아도, 컴퓨터를 꺼도 송출이 유지됩니다. 상태 확인과 중지는 다시 로그인해서 하면 됩니다.",
      ],
    },
    {
      heading: "24시간 방송을 만드는 순서",
      body: [],
      points: [
        "요금제를 선택하고 계정을 만듭니다",
        "반복 재생할 영상을 업로드합니다 — 서버가 방송용 규격으로 변환합니다",
        "YouTube 계정을 연결하거나, 스트림 키를 등록합니다",
        "반복 재생을 켜고 방송을 시작하거나, 시작 시각을 예약합니다",
      ],
    },
  ],
  faq: [
    {
      q: "유튜브는 24시간 연속 라이브를 허용하나요?",
      a: "YouTube는 장시간 라이브 스트림을 지원합니다. 다만 채널의 상태와 정책 준수 여부는 YouTube가 판단하므로, 저작권이 있는 음원이나 영상을 쓰면 채널 쪽에서 제한을 받을 수 있습니다. 247streams는 송출을 담당하며 채널 정책 결과를 보장하지는 않습니다.",
    },
    {
      q: "하나의 영상을 계속 반복 재생할 수 있나요?",
      a: "네. 반복 재생을 켜면 영상이 끝나는 즉시 처음부터 다시 송출합니다.",
    },
    {
      q: "방송이 끊기면 알 수 있나요?",
      a: "방송 화면에 현재 상태와 재연결 횟수가 표시되고, 시작·종료 시각과 오류가 송출 기록에 남습니다.",
    },
    {
      q: "동시에 몇 개 채널을 24시간 돌릴 수 있나요?",
      a: "요금제에 따라 다릅니다. Basic은 1개, Pro는 2개, Business는 3개를 동시에 송출할 수 있습니다.",
    },
  ],
  related: ["/playlist-live/", "/youtube-live-streaming/", "/pricing/"],
  indexable: true,
};

const PLAYLIST: PublicPage = {
  path: "/playlist-live/",
  title: "여러 영상을 이어서 내보내는 플레이리스트 라이브 | 247streams",
  description:
    "여러 개의 영상 파일을 하나의 플레이리스트로 묶어 순서대로 라이브 송출합니다. 항목 사이가 매끄럽게 이어지도록 서버가 같은 규격으로 변환하고, 반복과 예약 시작을 지원합니다.",
  h1: "여러 영상을 이어 붙여 하나의 라이브로 내보내기",
  lead: "트랙이 여러 개인 음악 방송이나 장면이 바뀌는 앰비언스 방송은 영상 하나로 만들 수 없습니다. 여기서는 파일을 그대로 두고 순서만 정합니다.",
  sections: [
    {
      heading: "플레이리스트 송출이란",
      body: [
        "업로드한 영상 여러 개를 순서대로 지정하면, 서버가 첫 번째 항목부터 차례로 하나의 라이브 스트림에 실어 보냅니다. YouTube 쪽에서는 처음부터 끝까지 끊기지 않은 하나의 방송으로 보입니다.",
        "항목이 바뀌는 지점이 매끄럽게 이어지려면 모든 항목이 같은 해상도·프레임레이트·코덱이어야 합니다. 247streams는 업로드한 파일을 방송용 규격으로 변환하면서 이 조건을 맞추고, 조건이 맞지 않는 조합은 방송을 시작하기 전에 거부합니다.",
      ],
    },
    {
      heading: "할 수 있는 구성",
      body: [],
      points: [
        "항목 순서 지정과 변경",
        "플레이리스트 전체 반복",
        "시작 시각 예약",
        "방송별로 다른 구성 저장 — 저장 가능한 방송 수는 요금제에 따라 3개에서 30개입니다",
      ],
    },
    {
      heading: "저장 용량 계산",
      body: [
        "같은 파일을 여러 방송에서 함께 쓸 수 있습니다. 저장 용량은 업로드한 파일을 기준으로 계산하며, 요금제에 따라 총 15GB에서 60GB입니다.",
      ],
    },
  ],
  faq: [
    {
      q: "항목이 바뀔 때 방송이 끊기나요?",
      a: "항목들이 같은 규격으로 변환되어 있으면 하나의 스트림으로 이어서 전송됩니다. 규격이 맞지 않는 조합은 방송 시작 단계에서 거부하고 어떤 항목이 문제인지 알려줍니다.",
    },
    {
      q: "한 방송에 영상을 몇 개까지 넣을 수 있나요?",
      a: "개수 자체에 고정된 상한은 없고, 업로드한 파일의 총 용량이 요금제의 저장 한도 안에 있으면 됩니다.",
    },
    {
      q: "플레이리스트를 방송 중에 바꿀 수 있나요?",
      a: "방송 중인 플레이리스트의 구성 변경은 다음 방송에 적용됩니다. 지금 송출 중인 내용에는 반영되지 않습니다.",
    },
  ],
  related: ["/youtube-24-live/", "/youtube-live-streaming/", "/pricing/"],
  indexable: true,
};

const LIVE_STREAMING: PublicPage = {
  path: "/youtube-live-streaming/",
  title: "YouTube 라이브 스트리밍을 서버에서 실행 | 247streams",
  description:
    "YouTube 계정 연결 또는 RTMP 스트림 키 등록만으로 라이브 스트리밍을 서버에서 실행합니다. 예약 시작, 자동 재연결, 송출 기록을 제공하며 설치할 프로그램은 없습니다.",
  h1: "YouTube 라이브 스트리밍, 내 PC가 아닌 서버에서",
  lead: "라이브 스트리밍에 필요한 인코딩과 전송을 서버가 담당합니다. 이용자는 무엇을, 어디로, 언제 보낼지만 정합니다.",
  sections: [
    {
      heading: "송출 대상을 등록하는 두 가지 방법",
      body: [
        "YouTube 계정을 연결하면 라이브 방송 생성과 스트림 키 발급을 247streams가 처리합니다. 구글 계정 로그인으로 권한을 위임하는 방식이며, 비밀번호는 전달되지 않습니다.",
        "직접 관리하는 스트림 키가 있다면 RTMP 주소와 키를 등록해서 쓸 수 있습니다. 등록한 키는 암호화해 보관하며 화면에 다시 표시하지 않습니다.",
      ],
    },
    {
      heading: "제공하는 송출 기능",
      body: [],
      points: [
        "영상 하나 또는 플레이리스트 송출, 반복 재생",
        "시작 시각 예약 — 지정한 시각에 서버가 송출을 시작합니다",
        "연결이 끊어지면 자동으로 재연결 시도",
        "송출 기록 — 시작·종료 시각, 전송량, 오류",
        "요금제에 따라 1~3개 방송 동시 송출",
      ],
    },
    {
      heading: "하지 않는 일",
      body: [
        "247streams는 미리 올려둔 영상을 송출하는 서비스입니다. 웹캠이나 화면을 실시간으로 캡처해 보내는 기능, 실시간 채팅 송출, 영상 편집 기능은 제공하지 않습니다.",
      ],
    },
  ],
  faq: [
    {
      q: "실시간 카메라 방송도 되나요?",
      a: "아니요. 업로드해 둔 영상 파일을 송출하는 서비스이며, 실시간 캡처는 지원하지 않습니다.",
    },
    {
      q: "YouTube 외의 플랫폼으로도 보낼 수 있나요?",
      a: "RTMP 주소와 스트림 키를 직접 등록하는 방식이면 다른 플랫폼으로도 전송할 수 있습니다. 계정 연결로 방송 생성까지 자동화되는 대상은 YouTube입니다.",
    },
    {
      q: "스트림 키는 안전하게 보관되나요?",
      a: "등록한 스트림 키는 암호화해 저장하고, 저장한 뒤에는 화면에 다시 표시하지 않습니다.",
    },
    {
      q: "화질과 비트레이트는 어떻게 정해지나요?",
      a: "업로드한 영상을 기준으로 서버가 방송용 규격으로 변환해 송출합니다.",
    },
  ],
  related: ["/youtube-24-live/", "/playlist-live/", "/pricing/"],
  indexable: true,
};

const PRICING: PublicPage = {
  path: "/pricing/",
  title: "요금제 — 247streams 24시간 송출 플랜",
  description:
    "Basic 월 19,900원, Pro 월 39,900원, Business 월 59,900원. 동시 송출 수와 저장 용량이 다르고, 모든 요금제에서 24시간 송출·플레이리스트·예약을 사용할 수 있습니다.",
  h1: "요금제",
  lead: "세 요금제의 차이는 동시에 송출할 수 있는 방송 수와 저장 용량입니다. 송출 기능 자체는 모든 요금제에서 같습니다.",
  sections: [
    {
      heading: "모든 요금제에 포함된 기능",
      body: [],
      points: [...PLAN_INCLUDES],
    },
    {
      heading: "결제와 해지",
      body: [
        "결제는 PayApp을 통한 월 정기 결제로 진행됩니다. 해지하면 이미 결제된 기간이 끝날 때까지 송출을 계속 사용할 수 있고, 그 이후에는 새 방송을 시작할 수 없습니다.",
        "요금제가 끝나더라도 업로드한 영상이 즉시 삭제되지는 않습니다.",
      ],
    },
  ],
  faq: [
    {
      q: "요금제별로 무엇이 다른가요?",
      a: "동시에 송출할 수 있는 방송 수(1~3개), 총 저장 용량(15~60GB), 파일 하나의 최대 크기(10~20GB), 저장할 수 있는 방송 수(3~30개)가 다릅니다.",
    },
    {
      q: "결제 수단은 무엇인가요?",
      a: "PayApp 결제창을 통해 처리되며, 카드 정보는 PayApp에서 직접 입력합니다. 247streams는 카드 정보를 저장하지 않습니다.",
    },
    {
      q: "요금제를 바꿀 수 있나요?",
      a: "다른 요금제로 새로 결제할 수 있습니다. 진행 중인 구독이 있으면 먼저 정리한 뒤 변경합니다.",
    },
    {
      q: "해지하면 방송이 바로 멈추나요?",
      a: "이미 송출 중인 방송이 해지 때문에 즉시 중단되지는 않습니다. 결제된 기간이 끝나면 새 방송을 시작할 수 없게 됩니다.",
    },
  ],
  related: ["/", "/youtube-24-live/", "/playlist-live/"],
  indexable: true,
};

/** Order matters: it is the sitemap's order and the footer's order. */
export const PUBLIC_PAGES: PublicPage[] = [
  HOME,
  YOUTUBE_24,
  PLAYLIST,
  LIVE_STREAMING,
  PRICING,
];

export function pageFor(pathname: string): PublicPage | null {
  const path = normalisePath(pathname);
  return PUBLIC_PAGES.find((p) => p.path === path) ?? null;
}

/**
 * One spelling for a path, so `/pricing`, `/pricing/` and `/pricing//` are the
 * same page to the client. The canonical in the HTML still names one of them.
 */
export function normalisePath(pathname: string): string {
  const stripped = pathname.replace(/\/+$/, "");
  return stripped === "" ? "/" : `${stripped}/`;
}

/** Short labels for the footer and the inline related links. */
export const PATH_LABELS: Record<string, string> = {
  "/": "홈",
  "/youtube-24-live/": "유튜브 24시간 라이브",
  "/playlist-live/": "플레이리스트 송출",
  "/youtube-live-streaming/": "라이브 스트리밍",
  "/pricing/": "요금제",
  "/terms/": "이용약관",
  "/privacy/": "개인정보처리방침",
};

/**
 * Paths a crawler should not spend its budget on, and should not index.
 *
 * These are not secrets — the server decides what a request may see, and it
 * does so again on every request. This list only keeps signed-out renderings of
 * private screens out of the index.
 */
export const PRIVATE_PATHS = [
  "/admin",
  "/billing/complete",
  "/api/",
] as const;

/* ---------------------------------------------------------------------- *\
   The two legal documents.

   They are prerendered as well — not because they rank for anything, but
   because a URL that exists has to answer with its own title and its own
   canonical. Left out of the prerender they would be served the home page's
   HTML, and Google would file them as duplicates of the home page.
\* ---------------------------------------------------------------------- */

export interface LegalSeo {
  path: string;
  which: "terms" | "privacy";
  title: string;
  description: string;
}

export const LEGAL_PAGES: LegalSeo[] = [
  {
    path: "/terms/",
    which: "terms",
    title: "이용약관 | 247streams",
    description:
      "247streams 서비스 이용약관 페이지입니다. 정식 약관은 준비 중이며, 약관에 포함될 항목과 현재의 서비스 범위를 밝힙니다.",
  },
  {
    path: "/privacy/",
    which: "privacy",
    title: "개인정보처리방침 | 247streams",
    description:
      "247streams가 실제로 저장하는 정보와 보관 방식을 정리한 개인정보처리방침 페이지입니다. 스트림 키와 연동 토큰은 암호화해 보관합니다.",
  },
];

/** Every URL the sitemap lists, in order. Only indexable, public pages. */
export function sitemapPaths(): string[] {
  return [
    ...PUBLIC_PAGES.filter((p) => p.indexable).map((p) => p.path),
    ...LEGAL_PAGES.map((p) => p.path),
  ];
}

/** Every path the prerender writes a file for. */
export function prerenderedPaths(): string[] {
  return [...PUBLIC_PAGES.map((p) => p.path), ...LEGAL_PAGES.map((p) => p.path)];
}
