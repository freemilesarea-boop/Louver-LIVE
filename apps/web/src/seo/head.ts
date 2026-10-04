/**
 * The `<head>` of every public page, `robots.txt` and `sitemap.xml`.
 *
 * Strings, not JSX, because the only consumer that matters is the build-time
 * prerender: these tags have to exist in the HTML the server sends, before any
 * script runs, or a crawler that does not render JavaScript sees none of them.
 *
 * Every structured-data object below describes something that is visible on the
 * same page. There is no rating, no review and no customer count anywhere in
 * this file, because none of those exist to describe.
 */
import {
  LEGAL_PAGES,
  PRIVATE_PATHS,
  PUBLIC_PAGES,
  PUBLIC_PLANS,
  SITE_NAME,
  SITE_ORIGIN,
  canonicalFor,
  sitemapPaths,
} from "./content";
import type { LegalSeo, PublicPage } from "./content";
import { LEGAL_CONTENT } from "../pages/legal-content";

/** What the head builder needs, whichever kind of page it is. */
export interface HeadInput {
  path: string;
  title: string;
  description: string;
  indexable: boolean;
  /** The page's own name, for the breadcrumb. The `<h1>`, not the `<title>`. */
  shortTitle: string;
  faq?: { q: string; a: string }[];
}

export function headInputFor(page: PublicPage | LegalSeo): HeadInput {
  if ("h1" in page) {
    return {
      path: page.path,
      title: page.title,
      description: page.description,
      indexable: page.indexable,
      shortTitle: page.h1,
      faq: page.faq,
    };
  }
  return {
    path: page.path,
    title: page.title,
    description: page.description,
    // A legal page is thin on purpose, but it is a real page with its own URL
    // and its own canonical, so it is indexable rather than hidden.
    indexable: true,
    shortTitle: LEGAL_CONTENT[page.which].title,
  };
}

/** HTML-escapes text going into an attribute or a text node. */
export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;")
    .replace(/'/g, "&#39;");
}

/**
 * Escapes a JSON-LD payload.
 *
 * Only `<` needs it, and only because `</script` inside a string would end the
 * element early. `JSON.stringify` has already quoted everything else.
 */
function jsonLd(value: unknown): string {
  const json = JSON.stringify(value).replace(/</g, "\\u003c");
  return `<script type="application/ld+json">${json}</script>`;
}

const ORGANISATION = {
  "@type": "Organization",
  "@id": `${SITE_ORIGIN}/#organization`,
  name: SITE_NAME,
  url: `${SITE_ORIGIN}/`,
  description:
    "업로드한 영상을 클라우드 서버에서 YouTube로 송출하는 24시간 라이브 송출 서비스",
};

const WEBSITE = {
  "@type": "WebSite",
  "@id": `${SITE_ORIGIN}/#website`,
  name: SITE_NAME,
  url: `${SITE_ORIGIN}/`,
  inLanguage: "ko-KR",
  publisher: { "@id": `${SITE_ORIGIN}/#organization` },
};

/**
 * The product, with its real prices.
 *
 * `offers` is built from `PUBLIC_PLANS`, which a test holds equal to the
 * server's own plan table — so the price in the search result, the price on the
 * page and the price PayApp is asked for are one number.
 */
function softwareApplication() {
  return {
    "@type": "SoftwareApplication",
    "@id": `${SITE_ORIGIN}/#service`,
    name: SITE_NAME,
    url: `${SITE_ORIGIN}/`,
    applicationCategory: "MultimediaApplication",
    applicationSubCategory: "라이브 스트리밍 송출",
    operatingSystem: "Web",
    inLanguage: "ko-KR",
    description:
      "영상을 업로드하면 서버가 YouTube로 24시간 라이브를 송출합니다. 플레이리스트 송출, 예약 시작, 자동 재연결을 제공합니다.",
    publisher: { "@id": `${SITE_ORIGIN}/#organization` },
    offers: PUBLIC_PLANS.map((p) => ({
      "@type": "Offer",
      name: p.label,
      price: String(p.monthlyKrw),
      priceCurrency: "KRW",
      category: "월 구독",
      url: canonicalFor("/pricing/"),
    })),
  };
}

function faqPage(path: string, faq: { q: string; a: string }[]) {
  return {
    "@type": "FAQPage",
    "@id": `${canonicalFor(path)}#faq`,
    mainEntity: faq.map((f) => ({
      "@type": "Question",
      name: f.q,
      acceptedAnswer: { "@type": "Answer", text: f.a },
    })),
  };
}

function breadcrumb(path: string, title: string) {
  return {
    "@type": "BreadcrumbList",
    "@id": `${canonicalFor(path)}#breadcrumb`,
    itemListElement: [
      {
        "@type": "ListItem",
        position: 1,
        name: SITE_NAME,
        item: `${SITE_ORIGIN}/`,
      },
      { "@type": "ListItem", position: 2, name: title, item: canonicalFor(path) },
    ],
  };
}

/** Everything in one `@graph`, which is one script tag instead of four. */
export function structuredData(input: HeadInput): string {
  const graph: unknown[] = [ORGANISATION, WEBSITE];
  if (input.path === "/" || input.path === "/pricing/") {
    graph.push(softwareApplication());
  }
  if (input.faq && input.faq.length > 0) {
    graph.push(faqPage(input.path, input.faq));
  }
  if (input.path !== "/") {
    graph.push(breadcrumb(input.path, input.shortTitle));
  }
  return jsonLd({ "@context": "https://schema.org", "@graph": graph });
}

/**
 * The head of one page, as HTML.
 *
 * `canonical` is always absolute and always on the production origin, in the
 * one spelling the server serves: with the trailing slash. A query string never
 * reaches it, so `?utm_source=...` and the bare URL collapse to one page.
 */
export function headFor(input: HeadInput): string {
  const url = canonicalFor(input.path);
  const title = escapeHtml(input.title);
  const description = escapeHtml(input.description);
  const robots = input.indexable
    ? "index,follow,max-image-preview:large"
    : "noindex,follow";
  return [
    `<title>${title}</title>`,
    `<meta name="description" content="${description}" />`,
    `<meta name="robots" content="${robots}" />`,
    `<link rel="canonical" href="${url}" />`,
    `<meta property="og:type" content="website" />`,
    `<meta property="og:site_name" content="${SITE_NAME}" />`,
    `<meta property="og:locale" content="ko_KR" />`,
    `<meta property="og:title" content="${title}" />`,
    `<meta property="og:description" content="${description}" />`,
    `<meta property="og:url" content="${url}" />`,
    // `summary` and not `summary_large_image`: there is no share image yet, and
    // naming a card size we cannot fill would just render an empty box.
    `<meta name="twitter:card" content="summary" />`,
    `<meta name="twitter:title" content="${title}" />`,
    `<meta name="twitter:description" content="${description}" />`,
    structuredData(input),
  ].join("\n    ");
}

/* ------------------------------------------------------------ robots.txt */

/**
 * `robots.txt`.
 *
 * The disallowed paths are not protected by this file — the server checks every
 * request again, and `/admin` refuses a member whether or not a crawler was
 * told to stay away. What this does is keep the signed-out rendering of private
 * screens out of the index, and keep the crawl budget on the pages that have
 * something to read.
 */
export function robotsTxt(): string {
  return [
    "User-agent: *",
    "Allow: /",
    ...PRIVATE_PATHS.map((p) => `Disallow: ${p}`),
    "",
    `Sitemap: ${SITE_ORIGIN}/sitemap.xml`,
    "",
  ].join("\n");
}

/* ----------------------------------------------------------- sitemap.xml */

/**
 * `sitemap.xml`.
 *
 * `<loc>` and nothing else. `lastmod` would have to be a real content date and
 * the build does not know one — stamping every URL with the build time on every
 * deploy is a lie that search engines learn to ignore. `changefreq` and
 * `priority` Google states it does not use.
 */
export function sitemapXml(): string {
  const urls = sitemapPaths()
    .map((p) => `  <url><loc>${canonicalFor(p)}</loc></url>`)
    .join("\n");
  return [
    '<?xml version="1.0" encoding="UTF-8"?>',
    '<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">',
    urls,
    "</urlset>",
    "",
  ].join("\n");
}

/** Every page the prerender writes, with the head it gets. */
export function allHeadInputs(): HeadInput[] {
  return [
    ...PUBLIC_PAGES.map((p) => headInputFor(p)),
    ...LEGAL_PAGES.map((p) => headInputFor(p)),
  ];
}
