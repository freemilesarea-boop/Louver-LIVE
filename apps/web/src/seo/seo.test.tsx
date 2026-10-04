/**
 * The public pages, the tags they carry, and the app they live in front of.
 *
 * Three groups:
 *  - the content and the head builders, which is what ends up in the HTML;
 *  - the pages as rendered, which is what a visitor and a rendering crawler see;
 *  - `App.tsx`'s routing, which is where a mistake would cost the signed-in
 *    application rather than a search ranking.
 *
 * The prices are checked against `SEED_PLANS` in the server's own source, by
 * parsing it. That is the only honest way to assert it: the pages are written
 * at build time, when there is no API to ask.
 */
import { render, screen, waitFor, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { ReactElement } from "react";
import { App } from "../App";
import { TransportProvider } from "../TransportContext";
import type { Transport } from "../transport";
import type { Me } from "../cloud";
import { GIB, seedPlans } from "../../../../scripts/seo-plan-source.mjs";
import {
  LEGAL_PAGES,
  PATH_LABELS,
  PRIVATE_PATHS,
  PUBLIC_PAGES,
  PUBLIC_PLANS,
  SITE_ORIGIN,
  normalisePath,
  pageFor,
  prerenderedPaths,
  sitemapPaths,
} from "./content";
import { headFor, headInputFor, robotsTxt, sitemapXml } from "./head";
import { LegalArticle, MarketingPage, PlanCards, PublicNotFound } from "./Marketing";

/* ------------------------------------------------------------- the content */

describe("the public route table", () => {
  it("gives every page its own path, title and description", () => {
    const paths = PUBLIC_PAGES.map((p) => p.path);
    const titles = PUBLIC_PAGES.map((p) => p.title);
    const descriptions = PUBLIC_PAGES.map((p) => p.description);
    expect(new Set(paths).size).toBe(paths.length);
    expect(new Set(titles).size).toBe(titles.length);
    expect(new Set(descriptions).size).toBe(descriptions.length);
  });

  it("spells every path the way the server serves it", () => {
    for (const path of prerenderedPaths()) {
      expect(path.startsWith("/")).toBe(true);
      // The trailing slash is the real URL — `ServeDir` redirects to it — so
      // every canonical, link and sitemap entry has to carry it.
      if (path !== "/") expect(path.endsWith("/")).toBe(true);
      expect(path).not.toContain("//");
      expect(normalisePath(path.replace(/\/$/, ""))).toBe(path);
    }
  });

  it("links only to pages that exist, and labels every one of them", () => {
    const known = new Set(prerenderedPaths());
    for (const page of PUBLIC_PAGES) {
      for (const link of page.related) {
        expect(known.has(link)).toBe(true);
        expect(PATH_LABELS[link]).toBeTruthy();
      }
      expect(page.related).not.toContain(page.path);
    }
    for (const path of known) expect(PATH_LABELS[path]).toBeTruthy();
  });

  it("gives every page a heading, a lead and some questions", () => {
    for (const page of PUBLIC_PAGES) {
      expect(page.h1.length).toBeGreaterThan(1);
      expect(page.lead.length).toBeGreaterThan(20);
      expect(page.faq.length).toBeGreaterThanOrEqual(3);
      for (const f of page.faq) expect(f.a.length).toBeGreaterThan(15);
    }
  });

  it("asks a different set of questions on each page", () => {
    const questions = PUBLIC_PAGES.flatMap((p) => p.faq.map((f) => f.q));
    expect(new Set(questions).size).toBe(questions.length);
  });
});

describe("the prices on the public pages", () => {
  it("are the prices the server charges", async () => {
    const plans = await seedPlans(process.cwd());
    for (const plan of PUBLIC_PLANS) {
      const seeded = plans[plan.id];
      expect(seeded, `${plan.id} is not in SEED_PLANS`).toBeTruthy();
      expect(plan.label).toBe(seeded!.label);
      expect(plan.monthlyKrw).toBe(seeded!.monthly_price_krw);
      expect(plan.concurrent).toBe(seeded!.limits.max_concurrent_streams);
      expect(plan.broadcasts).toBe(seeded!.limits.max_broadcasts);
      expect(plan.storageGb * GIB).toBe(seeded!.limits.max_storage_bytes);
      expect(plan.uploadGb * GIB).toBe(seeded!.limits.max_upload_bytes);
    }
  });

  it("covers every plan that is on sale, and nothing that is not", async () => {
    const plans = await seedPlans(process.cwd());
    const onSale = Object.entries(plans)
      .filter(([, p]) => p.active)
      .map(([id]) => id)
      .sort();
    expect(PUBLIC_PLANS.map((p) => p.id).sort()).toEqual(onSale);
  });
});

/* ---------------------------------------------------------------- the head */

describe("the head of a public page", () => {
  const head = (path: string) => {
    const page = PUBLIC_PAGES.find((p) => p.path === path);
    return headFor(headInputFor(page!));
  };

  it("carries one title, one description and one canonical", () => {
    const html = head("/youtube-24-live/");
    expect(html.match(/<title>/g)).toHaveLength(1);
    expect(html.match(/name="description"/g)).toHaveLength(1);
    expect(html).toContain(
      `<link rel="canonical" href="${SITE_ORIGIN}/youtube-24-live/" />`,
    );
  });

  it("puts the production origin on every absolute URL", () => {
    for (const page of [...PUBLIC_PAGES, ...LEGAL_PAGES]) {
      const html = headFor(headInputFor(page));
      for (const [, url] of html.matchAll(/href="(https?:[^"]+)"/g)) {
        expect(url!.startsWith(`${SITE_ORIGIN}/`)).toBe(true);
      }
      expect(html).toContain(`content="${SITE_ORIGIN}${page.path}"`);
    }
  });

  it("asks to be indexed, and says so once", () => {
    const html = head("/pricing/");
    expect(html.match(/name="robots"/g)).toHaveLength(1);
    expect(html).toContain('content="index,follow,max-image-preview:large"');
  });

  it("escapes what it puts in an attribute", () => {
    const html = headFor({
      path: "/pricing/",
      title: 'a "quoted" <title>',
      description: "a & b",
      indexable: true,
      shortTitle: "x",
    });
    expect(html).toContain("&quot;quoted&quot;");
    expect(html).toContain("a &amp; b");
    expect(html).not.toContain("<title>a \"");
  });

  it("describes the page in structured data, and invents nothing", () => {
    for (const page of PUBLIC_PAGES) {
      const html = headFor(headInputFor(page));
      const json = html.match(
        /<script type="application\/ld\+json">([\s\S]*?)<\/script>/,
      );
      expect(json, `${page.path} has no JSON-LD`).toBeTruthy();
      const graph = JSON.parse(json![1]!)["@graph"] as { "@type": string }[];
      expect(graph.map((n) => n["@type"])).toContain("Organization");
      expect(graph.map((n) => n["@type"])).toContain("WebSite");
      const serialised = JSON.stringify(graph);
      for (const invented of ["aggregateRating", "ratingValue", "Review", "review"]) {
        expect(serialised).not.toContain(invented);
      }
      const faq = graph.find((n) => n["@type"] === "FAQPage") as
        | { mainEntity: { name: string; acceptedAnswer: { text: string } }[] }
        | undefined;
      // Every question in the schema is a question on the page, which is the
      // rule Google actually enforces.
      expect(faq?.mainEntity.map((q) => q.name)).toEqual(page.faq.map((f) => f.q));
      expect(faq?.mainEntity.map((q) => q.acceptedAnswer.text)).toEqual(
        page.faq.map((f) => f.a),
      );
    }
  });

  it("prices the product from the plan table", () => {
    const html = headFor(headInputFor(PUBLIC_PAGES.find((p) => p.path === "/pricing/")!));
    const graph = JSON.parse(
      html.match(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/)![1]!,
    )["@graph"] as { "@type": string; offers?: { name: string; price: string }[] }[];
    const app = graph.find((n) => n["@type"] === "SoftwareApplication");
    expect(app?.offers?.map((o) => [o.name, o.price])).toEqual(
      PUBLIC_PLANS.map((p) => [p.label, String(p.monthlyKrw)]),
    );
  });
});

describe("robots.txt and sitemap.xml", () => {
  it("declares the sitemap and keeps crawlers off the private paths", () => {
    const robots = robotsTxt();
    expect(robots).toContain(`Sitemap: ${SITE_ORIGIN}/sitemap.xml`);
    expect(robots).toMatch(/^User-agent: \*$/m);
    expect(robots).toMatch(/^Allow: \/$/m);
    for (const path of PRIVATE_PATHS) {
      expect(robots).toContain(`Disallow: ${path}`);
    }
    for (const page of PUBLIC_PAGES) {
      expect(robots).not.toMatch(new RegExp(`^Disallow: ${page.path}$`, "m"));
    }
  });

  it("lists every public page once, absolutely, and nothing private", () => {
    const xml = sitemapXml();
    const locs = [...xml.matchAll(/<loc>([^<]+)<\/loc>/g)].map((m) => m[1]!);
    expect(locs).toEqual(sitemapPaths().map((p) => `${SITE_ORIGIN}${p}`));
    expect(new Set(locs).size).toBe(locs.length);
    for (const loc of locs) {
      expect(loc.startsWith(`${SITE_ORIGIN}/`)).toBe(true);
      for (const path of PRIVATE_PATHS) {
        expect(loc.slice(SITE_ORIGIN.length).startsWith(path)).toBe(false);
      }
    }
  });

  it("leaves a page that is not indexable out of the sitemap", () => {
    for (const page of PUBLIC_PAGES) {
      if (page.indexable) continue;
      expect(sitemapPaths()).not.toContain(page.path);
    }
  });
});

/* --------------------------------------------------------------- the pages */

describe("a rendered public page", () => {
  it("has exactly one h1, and it is the page's own heading", () => {
    for (const page of PUBLIC_PAGES) {
      const { unmount } = render(<MarketingPage page={page} />);
      const headings = screen.getAllByRole("heading", { level: 1 });
      expect(headings).toHaveLength(1);
      expect(headings[0]).toHaveTextContent(page.h1);
      unmount();
    }
  });

  it("writes the questions and the answers into the markup", () => {
    const page = PUBLIC_PAGES.find((p) => p.path === "/youtube-24-live/")!;
    render(<MarketingPage page={page} />);
    for (const f of page.faq) {
      expect(screen.getByText(f.q)).toBeInTheDocument();
      expect(screen.getByText(f.a)).toBeInTheDocument();
    }
  });

  it("links to the other pages, so one of them found is all of them found", () => {
    const page = PUBLIC_PAGES.find((p) => p.path === "/playlist-live/")!;
    render(<MarketingPage page={page} />);
    for (const link of [...page.related, "/terms/", "/privacy/"]) {
      expect(
        document.querySelector(`a[href="${link}"]`),
        `no link to ${link}`,
      ).toBeTruthy();
    }
    // Nothing points at a URL that is not a page.
    for (const a of Array.from(document.querySelectorAll("a[href^='/']"))) {
      const href = a.getAttribute("href")!;
      expect(prerenderedPaths()).toContain(href);
    }
  });

  it("shows the plan numbers the plan table holds", () => {
    render(<PlanCards />);
    for (const plan of PUBLIC_PLANS) {
      const card = screen.getByRole("heading", { name: plan.label }).closest("section")!;
      expect(within(card).getByText(`₩${plan.monthlyKrw.toLocaleString("en-US")}`)).toBeInTheDocument();
      expect(within(card).getByText(`${plan.storageGb}GB`)).toBeInTheDocument();
      expect(within(card).getByText(`${plan.uploadGb}GB`)).toBeInTheDocument();
    }
  });

  it("renders a legal document from the same words the app shows", () => {
    render(<LegalArticle page={LEGAL_PAGES[0]!} />);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("이용약관");
    expect(screen.getByRole("note")).toHaveTextContent("준비 중인 초안");
  });

  it("says a URL does not exist rather than pretending it does", () => {
    render(<PublicNotFound path="/nope" />);
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(
      "페이지를 찾을 수 없습니다",
    );
    expect(screen.getByText("/nope")).toBeInTheDocument();
  });
});

/* ----------------------------------------------------------------- routing */

const ME: Me = {
  id: "u1",
  email: "a@b.kr",
  name: "운영자",
  role: "member",
  plan_id: "pro",
};

/** Only the methods the screens under test actually call. */
function transport(over: Partial<Transport> = {}): Transport {
  return {
    kind: "web",
    me: vi.fn().mockRejectedValue(new Error("signed out")),
    logout: vi.fn().mockResolvedValue(undefined),
    login: vi.fn(),
    register: vi.fn(),
    plans: vi.fn().mockResolvedValue([]),
    billingStatus: vi
      .fn()
      .mockResolvedValue({ subscription: null, provider: "payapp", configured: true }),
    listMedia: vi.fn().mockResolvedValue([]),
    listDestinations: vi.fn().mockResolvedValue([]),
    dashboard: vi
      .fn()
      .mockResolvedValue({ plan_label: "Pro", active: 0, allowed: 2, broadcasts: [] }),
    watchDashboard: vi.fn().mockReturnValue(() => undefined),
    health: vi.fn().mockResolvedValue({
      status: "ok",
      version: "test",
      deployment: "cloud",
      checks: { api: true, database: true, ffmpeg: true, ffmpeg_rtmps: true, storage: true },
    }),
    subscription: vi.fn().mockResolvedValue({ active: true }),
    ...over,
  } as unknown as Transport;
}

function at(path: string, t: Transport = transport()): ReactElement {
  window.history.replaceState({}, "", path);
  document.head.innerHTML = "";
  return <TransportProvider value={t}>{<App />}</TransportProvider>;
}

describe("where the app sends each URL", () => {
  it("shows a landing page without waiting for the session check", () => {
    const t = transport({ me: vi.fn(() => new Promise<Me>(() => undefined)) });
    render(at("/youtube-24-live/", t));
    // Not a spinner: the heading is there on the first paint.
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(
      "유튜브 24시간 라이브 방송",
    );
    expect(screen.queryByText("불러오는 중…")).not.toBeInTheDocument();
  });

  it("keeps the way in on the front page, under the marketing copy", async () => {
    render(at("/"));
    await waitFor(() =>
      expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(
        pageFor("/")!.h1,
      ),
    );
    // The sign-in form is still on `/`, which is where every existing member
    // expects it to be.
    expect(screen.getByLabelText("이메일")).toBeInTheDocument();
    expect(screen.getByLabelText("비밀번호")).toBeInTheDocument();
    // And only one h1 — the page's, not the form's.
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
  });

  it("shows the public price list to a visitor who is not signed in", async () => {
    render(at("/pricing/"));
    await waitFor(() =>
      expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("요금제"),
    );
    expect(screen.getByRole("heading", { name: "Basic" })).toBeInTheDocument();
  });

  it("still gives a signed-in member the app on its own URLs", async () => {
    const t = transport({ me: vi.fn().mockResolvedValue(ME) });
    render(at("/", t));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "로그아웃" })).toBeInTheDocument(),
    );
    // The app's tabs, not a marketing page.
    expect(screen.getByRole("button", { name: "방송" })).toBeInTheDocument();
    expect(screen.queryByText(pageFor("/")!.lead)).not.toBeInTheDocument();
  });

  it("still opens the pricing tab for a signed-in member", async () => {
    const t = transport({ me: vi.fn().mockResolvedValue(ME) });
    render(at("/pricing/", t));
    await waitFor(() =>
      expect(screen.getByRole("button", { name: "요금제" })).toHaveAttribute(
        "aria-current",
        "page",
      ),
    );
  });

  it("answers an unknown URL honestly, and asks not to be indexed for it", async () => {
    render(at("/no-such-page"));
    expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent(
      "페이지를 찾을 수 없습니다",
    );
    await waitFor(() =>
      expect(
        document.querySelector('meta[name="robots"]')?.getAttribute("content"),
      ).toBe("noindex,follow"),
    );
    // And it does not keep the home page's canonical, which would hand the
    // `noindex` to the home page.
    expect(document.querySelector('link[rel="canonical"]')).toBeNull();
  });

  it("marks the console noindex and leaves the refusal to the server", async () => {
    const t = transport({ me: vi.fn().mockResolvedValue(ME) });
    render(at("/admin", t));
    await waitFor(() =>
      expect(
        document.querySelector('meta[name="robots"]')?.getAttribute("content"),
      ).toBe("noindex,follow"),
    );
    // A member typing the URL is still told no — unchanged by any of this.
    expect(screen.getByText("관리자만 열 수 있는 페이지입니다.")).toBeInTheDocument();
  });

  it("does not touch the robots tag the server sent for a real page", async () => {
    document.head.innerHTML =
      '<meta name="robots" content="index,follow"><link rel="canonical" href="https://247streams.kr/pricing/">';
    window.history.replaceState({}, "", "/pricing/");
    render(
      <TransportProvider value={transport()}>
        <App />
      </TransportProvider>,
    );
    await waitFor(() =>
      expect(screen.getByRole("heading", { level: 1 })).toHaveTextContent("요금제"),
    );
    expect(
      document.querySelector('meta[name="robots"]')?.getAttribute("content"),
    ).toBe("index,follow");
    expect(document.querySelector('link[rel="canonical"]')).not.toBeNull();
  });
});
