/**
 * What the build-time prerender imports.
 *
 * Vite builds this file once, for Node (`vite.seo.config.ts`), and
 * `scripts/seo-prerender.mjs` calls the two functions below to write the HTML
 * every public URL answers with. React and `react-dom/server` stay external, so
 * the output is these components and nothing else; no browser bundle grows by a
 * byte because of this file.
 *
 * `renderToStaticMarkup` and not `renderToString`: the browser mounts with
 * `createRoot`, which replaces whatever is in `#root` rather than hydrating it,
 * so the hydration bookkeeping would be bytes on the wire for nothing.
 */
import { renderToStaticMarkup } from "react-dom/server";
import { LEGAL_PAGES, PUBLIC_PAGES } from "./content";
import { headFor, headInputFor } from "./head";
import { LegalArticle, MarketingPage } from "./Marketing";

export interface PrerenderedPage {
  /** The URL this HTML answers, with its trailing slash. */
  path: string;
  /** The `<head>` tags, as HTML. */
  head: string;
  /** What goes inside `#root`, as HTML. */
  body: string;
  /** False for a page that must not be indexed, for the checker to assert on. */
  indexable: boolean;
}

export function prerender(): PrerenderedPage[] {
  const marketing = PUBLIC_PAGES.map((page) => ({
    path: page.path,
    head: headFor(headInputFor(page)),
    body: renderToStaticMarkup(<MarketingPage page={page} />),
    indexable: page.indexable,
  }));
  const legal = LEGAL_PAGES.map((page) => ({
    path: page.path,
    head: headFor(headInputFor(page)),
    body: renderToStaticMarkup(<LegalArticle page={page} />),
    indexable: true,
  }));
  return [...marketing, ...legal];
}

export { robotsTxt, sitemapXml } from "./head";
export { SITE_ORIGIN } from "./content";
