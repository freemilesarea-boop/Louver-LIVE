/**
 * The text of the two documents signup links to, as data.
 *
 * Separated from `Legal.tsx` so that the build-time prerender can render these
 * pages into static HTML without importing the signed-in application. The page
 * component and the prerender therefore show the same words.
 *
 * Every statement about data is meant to match what the code does today:
 * the tables in `crates/louver-cloud`, the 30-day session in
 * `apps/server/src/auth.rs`, the single `youtube.force-ssl` scope, PayApp for
 * billing, and `disconnect()` deleting the sealed YouTube tokens. Change the
 * code and these words together.
 *
 * When the wording of the terms changes materially, bump `TERMS_VERSION` in
 * `apps/server/src/auth.rs` so that existing accounts can be asked again.
 */

/**
 * Who runs the service. A field left `null` is simply not printed — the page
 * never shows an invented name or address. Fill these in before submitting
 * the YouTube API audit; Korean law expects the business name, representative
 * and a contact on the privacy page.
 */
export const OPERATOR = {
  service: "247streams",
  site: "https://247streams.kr",
  businessName: null as string | null,
  representative: null as string | null,
  registrationNumber: null as string | null,
  address: null as string | null,
  email: null as string | null,
  /** Where the servers and the database physically are, e.g. "독일". */
  hostingCountry: null as string | null,
};

export const EFFECTIVE_DATE = "2026년 11월 27일";

export interface LegalSection {
  heading: string;
  paragraphs?: readonly string[];
  items?: readonly string[];
  links?: readonly { label: string; href: string }[];
}

export interface LegalDoc {
  title: string;
  summary: string;
  sections: readonly LegalSection[];
}

const YT_TOS = "https://www.youtube.com/t/terms";
const GOOGLE_PRIVACY = "https://policies.google.com/privacy";
const GOOGLE_PERMISSIONS = "https://myaccount.google.com/permissions";

const terms: LegalDoc = {
  title: "이용약관",
  summary:
    "247streams는 이용자가 업로드한 영상을 클라우드 서버에서 이용자의 YouTube 채널로 실시간 송출하는 서비스입니다. 이 약관은 서비스 이용에 관한 회사와 이용자의 권리·의무를 정합니다.",
  sections: [
    {
      heading: "제1조 (목적)",
      paragraphs: [
        "이 약관은 247streams(이하 \"서비스\")를 운영하는 운영자(이하 \"회사\")와 서비스를 이용하는 회원(이하 \"이용자\") 사이의 권리, 의무 및 책임 사항을 정하는 것을 목적으로 합니다.",
      ],
    },
    {
      heading: "제2조 (약관의 효력과 변경)",
      items: [
        "이 약관은 서비스 화면에 게시하고, 이용자가 회원가입 시 동의함으로써 효력이 생깁니다.",
        "회사는 관련 법령을 위반하지 않는 범위에서 약관을 변경할 수 있으며, 변경 시 적용일 7일 전(이용자에게 불리한 변경은 30일 전)부터 서비스 화면에 공지합니다.",
        "변경된 약관에 동의하지 않는 이용자는 이용계약을 해지할 수 있습니다.",
      ],
    },
    {
      heading: "제3조 (회원가입과 계정)",
      items: [
        "이용자는 이름, 이메일, 비밀번호를 입력하고 이 약관과 개인정보처리방침에 동의하여 가입합니다.",
        "계정 정보는 이용자 본인이 관리하며, 타인에게 양도하거나 빌려줄 수 없습니다.",
        "타인의 정보를 도용하거나 허위 정보로 가입한 경우 회사는 계정을 제한하거나 해지할 수 있습니다.",
      ],
    },
    {
      heading: "제4조 (서비스의 내용)",
      items: [
        "영상 파일 업로드와 보관",
        "플레이리스트 구성과 24시간 반복 송출",
        "송출 예약",
        "YouTube 계정 연동을 통한 방송 생성, 제목·설명 변경, 채팅 메시지 자동 게시",
        "그 밖에 회사가 정하여 안내하는 기능",
      ],
      paragraphs: [
        "요금제별 저장 용량, 파일 크기, 동시 송출 수 등 이용 한도는 요금 안내 페이지에 게시된 내용을 따릅니다.",
      ],
    },
    {
      heading: "제5조 (YouTube 연동)",
      paragraphs: [
        "서비스의 YouTube 연동 기능은 YouTube API 서비스를 사용합니다. YouTube 계정을 연동하는 이용자는 YouTube 서비스 약관에 동의하는 것으로 봅니다. 연동은 언제든지 서비스 안에서 해제하거나 Google 계정 보안 설정에서 철회할 수 있습니다.",
      ],
      links: [
        { label: "YouTube 서비스 약관", href: YT_TOS },
        { label: "Google 개인정보처리방침", href: GOOGLE_PRIVACY },
      ],
    },
    {
      heading: "제6조 (요금과 결제)",
      items: [
        "유료 요금제의 요금은 요금 안내 페이지에 게시된 금액(부가가치세 포함)이며, 결제대행사(PayApp)를 통해 매월 자동으로 결제됩니다.",
        "결제가 실패하면 회사는 이용자에게 알리고, 결제가 이루어질 때까지 유료 기능의 이용을 제한할 수 있습니다.",
        "요금이 변경되는 경우 적용 30일 전에 공지하며, 변경된 요금은 다음 결제일부터 적용됩니다.",
      ],
    },
    {
      heading: "제7조 (해지와 환불)",
      items: [
        "이용자는 언제든지 자동결제를 해지할 수 있으며, 해지하면 이미 결제한 이용 기간이 끝날 때까지 서비스를 이용할 수 있습니다.",
        "결제일로부터 7일 이내이고 송출 등 유료 기능을 사용하지 않은 경우, 이용자의 요청에 따라 결제 금액 전액을 환불합니다.",
        "그 밖의 환불은 「전자상거래 등에서의 소비자보호에 관한 법률」 등 관련 법령에 따릅니다.",
        "회원 탈퇴는 아래 문의처로 요청할 수 있으며, 탈퇴 시 업로드한 영상과 계정 정보는 개인정보처리방침에 따라 삭제됩니다.",
      ],
    },
    {
      heading: "제8조 (이용자의 의무)",
      items: [
        "이용자는 업로드하고 송출하는 영상·음원·이미지에 대해 필요한 권리를 가지고 있어야 합니다.",
        "이용자는 YouTube 커뮤니티 가이드와 서비스 약관, 저작권법 등 관련 법령을 지켜야 합니다.",
        "서비스를 이용해 타인의 권리를 침해하거나, 불법·유해 콘텐츠를 송출하거나, 서비스 운영을 방해해서는 안 됩니다.",
        "스트림 키, 계정 비밀번호 등은 이용자가 안전하게 관리해야 합니다.",
      ],
    },
    {
      heading: "제9조 (콘텐츠의 권리)",
      paragraphs: [
        "이용자가 업로드한 콘텐츠의 권리는 이용자에게 있습니다. 회사는 서비스 제공(보관, 변환, 송출)에 필요한 범위에서만 콘텐츠를 처리하며, 다른 목적으로 사용하지 않습니다.",
        "권리 침해 신고가 접수되거나 법령 위반이 확인된 콘텐츠는 송출을 중단하거나 삭제할 수 있습니다.",
      ],
    },
    {
      heading: "제10조 (서비스의 중단)",
      paragraphs: [
        "회사는 설비 점검·교체, 장애, 천재지변, 외부 서비스(YouTube 등)의 정책 변경이나 장애 등의 사유로 서비스를 일시 중단할 수 있으며, 가능한 경우 사전에 공지합니다.",
        "YouTube API 일일 사용량 한도 등 외부 서비스의 제한으로 일부 기능이 일시적으로 동작하지 않을 수 있습니다.",
      ],
    },
    {
      heading: "제11조 (책임의 제한)",
      items: [
        "회사는 천재지변, 외부 서비스의 장애나 정책 변경, 이용자의 귀책 사유로 생긴 손해에 대해서는 책임을 지지 않습니다.",
        "이용자가 송출한 콘텐츠로 인해 생긴 분쟁(저작권 등)의 책임은 해당 이용자에게 있습니다.",
        "회사의 고의 또는 중대한 과실로 인한 손해에 대해서는 관련 법령에 따라 책임을 집니다.",
      ],
    },
    {
      heading: "제12조 (이용 제한)",
      paragraphs: [
        "이용자가 이 약관을 위반한 경우 회사는 경고, 송출 중단, 계정 정지 또는 해지 등의 조치를 할 수 있으며, 조치 사유를 이용자에게 알립니다.",
      ],
    },
    {
      heading: "제13조 (준거법과 관할)",
      paragraphs: [
        "이 약관은 대한민국 법률에 따르며, 서비스 이용과 관련한 분쟁은 「민사소송법」에 따른 관할 법원에서 해결합니다.",
      ],
    },
  ],
};

const privacy: LegalDoc = {
  title: "개인정보처리방침",
  summary:
    "247streams(이하 \"회사\")는 「개인정보 보호법」 등 관련 법령을 지키며, 서비스 제공에 필요한 최소한의 개인정보만 처리합니다. 이 방침은 회사가 어떤 정보를 왜, 얼마나 보관하고 어떻게 보호하는지 설명합니다.",
  sections: [
    {
      heading: "1. 처리하는 개인정보 항목",
      items: [
        "회원가입: 이름, 이메일 주소, 비밀번호(복원할 수 없는 해시 값으로만 저장), 약관 동의 시각",
        "로그인 유지: 로그인 세션 정보(세션 토큰의 해시 값, 만료 시각)",
        "서비스 이용: 업로드한 영상 파일과 파일 정보, 플레이리스트·예약·방송 설정, 송출 기록(시작·종료 시각, 전송량, 오류 내용)",
        "송출 대상: 이용자가 입력한 스트림 주소와 스트림 키(암호화하여 저장)",
        "YouTube 계정 연동 시: 채널 ID, 채널 이름, 채널 썸네일 주소, Google OAuth 액세스·갱신 토큰(암호화하여 저장)",
        "유료 결제 시: 요금제, 결제 금액, 결제 상태와 결제대행사가 발급한 거래 식별 정보. 카드번호 등 결제수단 정보는 회사가 저장하지 않으며 결제대행사가 처리합니다.",
      ],
    },
    {
      heading: "2. 개인정보의 이용 목적",
      items: [
        "회원 식별, 로그인, 계정 관리",
        "영상 보관과 YouTube 등 이용자가 지정한 대상으로의 송출",
        "이용자가 요청한 YouTube 방송 생성, 제목·설명 변경, 채팅 메시지 게시",
        "요금 결제와 정산, 환불 처리",
        "장애 대응, 부정 이용 방지, 문의 응대",
      ],
      paragraphs: [
        "회사는 개인정보를 광고나 마케팅, 프로파일링에 사용하지 않고 판매하지 않습니다.",
      ],
    },
    {
      heading: "3. 보유 및 이용 기간",
      items: [
        "계정 정보와 업로드한 콘텐츠: 회원 탈퇴 시까지. 탈퇴 요청 후 지체 없이 삭제합니다.",
        "로그인 세션: 발급 후 30일 또는 로그아웃 시까지",
        "YouTube 연동 정보와 토큰: 연동 해제 또는 회원 탈퇴 시 즉시 삭제",
        "계약·청약철회, 대금 결제 기록: 5년 (「전자상거래 등에서의 소비자보호에 관한 법률」)",
        "소비자 불만 또는 분쟁 처리 기록: 3년 (같은 법률)",
      ],
    },
    {
      heading: "4. 개인정보의 제3자 제공",
      paragraphs: [
        "회사는 이용자의 개인정보를 제3자에게 제공하지 않습니다. 다만 이용자가 직접 YouTube 연동과 송출을 요청한 경우, 그 기능을 수행하기 위해 방송 제목·설명, 영상 신호, 채팅 메시지 등 이용자가 지정한 내용이 YouTube(Google LLC)로 전송됩니다. 법령에 특별한 규정이 있는 경우는 예외로 합니다.",
      ],
    },
    {
      heading: "5. 개인정보 처리의 위탁",
      items: [
        "PayApp(결제대행): 유료 요금제의 정기 결제 처리",
        "클라우드 서버 제공업체: 서비스 서버와 데이터 보관",
      ],
    },
    {
      heading: "6. 개인정보의 국외 이전",
      paragraphs: [
        "YouTube 계정을 연동한 이용자의 경우, 위 4항의 내용이 이용자가 송출이나 방송 생성을 실행할 때 정보통신망을 통해 Google LLC(미국)로 전송됩니다. 이 전송은 이용자가 요청한 서비스 제공에 꼭 필요한 범위에 한하며, 연동을 해제하면 더 이상 전송되지 않습니다.",
      ],
    },
    {
      heading: "7. 개인정보의 파기",
      paragraphs: [
        "보유 기간이 끝나거나 처리 목적이 달성된 개인정보는 지체 없이 파기합니다. 전자 파일은 복구할 수 없는 방법으로 삭제하며, 법령에 따라 보관해야 하는 정보는 다른 정보와 분리하여 보관한 뒤 기간이 끝나면 파기합니다.",
      ],
    },
    {
      heading: "8. 이용자의 권리와 행사 방법",
      items: [
        "이용자는 언제든지 자신의 개인정보 열람, 정정, 삭제, 처리 정지를 요청할 수 있습니다.",
        "YouTube 연동은 서비스의 YouTube 계정 화면에서 바로 해제할 수 있으며, 해제하면 저장된 토큰과 채널 정보가 삭제됩니다.",
        "Google 계정 보안 설정(서드 파티 액세스)에서 247streams의 접근 권한을 직접 철회할 수도 있습니다.",
        "요청은 아래 문의처로 할 수 있으며, 회사는 지체 없이 조치합니다.",
      ],
      links: [{ label: "Google 계정 권한 관리", href: GOOGLE_PERMISSIONS }],
    },
    {
      heading: "9. 개인정보의 안전성 확보 조치",
      items: [
        "비밀번호는 복원할 수 없는 해시로만 저장합니다.",
        "스트림 키와 YouTube 토큰은 암호화하여 저장하고, 화면에 다시 표시하지 않습니다.",
        "모든 통신은 HTTPS로 암호화합니다.",
        "관리자 접근은 최소 인원으로 제한하고, 관리자 작업은 기록으로 남깁니다.",
      ],
    },
    {
      heading: "10. 쿠키의 사용",
      paragraphs: [
        "회사는 로그인 상태를 유지하기 위한 세션 쿠키 하나만 사용합니다. 광고나 분석 목적의 쿠키, 외부 추적 도구는 사용하지 않습니다. 브라우저 설정에서 쿠키를 거부할 수 있지만, 그 경우 로그인이 필요한 기능을 이용할 수 없습니다.",
      ],
    },
    {
      heading: "11. YouTube API 서비스 이용",
      paragraphs: [
        "247streams는 YouTube API 서비스를 사용합니다. 서비스를 이용하는 것은 YouTube 서비스 약관에 동의하는 것을 의미하며, YouTube 연동과 관련된 정보는 Google 개인정보처리방침에 따라서도 처리됩니다.",
        "회사는 Google OAuth를 통해 youtube.force-ssl 권한 하나만 요청하며, 이 권한은 이용자가 직접 실행한 기능(방송 생성과 송출 연결, 방송 제목·설명 변경, 채팅 메시지 게시, 방송 상태 확인)에만 사용합니다. YouTube API로 받은 정보를 광고에 쓰거나 다른 사람에게 판매·제공하지 않으며, Google API 서비스 사용자 데이터 정책(제한적 사용 요건 포함)을 지킵니다.",
        "YouTube에서 받아 저장한 채널 정보는 이용 중 갱신되며, 연동 해제나 권한 철회 시 삭제됩니다.",
      ],
      links: [
        { label: "YouTube 서비스 약관", href: YT_TOS },
        { label: "Google 개인정보처리방침", href: GOOGLE_PRIVACY },
        { label: "Google 계정 권한 관리", href: GOOGLE_PERMISSIONS },
        {
          label: "Google API 서비스 사용자 데이터 정책",
          href: "https://developers.google.com/terms/api-services-user-data-policy",
        },
      ],
    },
    {
      heading: "YouTube API Services (English summary)",
      paragraphs: [
        "247streams uses YouTube API Services. By using 247streams you agree to be bound by the YouTube Terms of Service. Data related to YouTube is also handled under the Google Privacy Policy.",
        "We request a single OAuth scope, youtube.force-ssl, and use it only for actions the user starts: creating a live broadcast and binding its stream, updating the broadcast title and description, posting chat messages the user wrote, and checking broadcast status. We store the channel ID, channel name, channel thumbnail URL and the OAuth tokens (encrypted). We do not sell this data, use it for advertising, or share it with third parties, and we comply with the Google API Services User Data Policy, including the Limited Use requirements.",
        "Users can disconnect their YouTube account inside 247streams at any time, which deletes the stored tokens and channel data immediately, or revoke access via the Google security settings page. Only a session cookie is used to keep users signed in; no advertising or analytics cookies are used.",
      ],
      links: [
        { label: "YouTube Terms of Service", href: YT_TOS },
        { label: "Google Privacy Policy", href: GOOGLE_PRIVACY },
        { label: "Revoke access (Google security settings)", href: GOOGLE_PERMISSIONS },
      ],
    },
    {
      heading: "12. 개인정보 보호책임자와 문의처",
      paragraphs: [
        "개인정보와 관련한 문의, 열람·삭제 요청, 불만 처리는 아래로 연락해 주세요. 개인정보 침해에 대한 상담은 개인정보침해신고센터(국번 없이 118), 개인정보분쟁조정위원회(1833-6972)에도 할 수 있습니다.",
      ],
    },
    {
      heading: "13. 방침의 변경",
      paragraphs: [
        "이 방침이 바뀌는 경우 시행 7일 전부터 서비스 화면에 공지합니다.",
      ],
    },
  ],
};

export const LEGAL_CONTENT: Record<"terms" | "privacy", LegalDoc> = { terms, privacy };

/** The operator lines that are actually filled in, label first. */
export function operatorLines(): [string, string][] {
  const o = OPERATOR;
  const rows: [string, string | null][] = [
    ["서비스", o.service],
    ["상호", o.businessName],
    ["대표자", o.representative],
    ["사업자등록번호", o.registrationNumber],
    ["주소", o.address],
    ["이메일", o.email],
    ["웹사이트", o.site],
  ];
  return rows.filter((r): r is [string, string] => !!r[1]);
}
