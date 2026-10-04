/**
 * Says `noindex` for a URL the server could not say it for.
 *
 * The server answers every path it has no file for with the app shell and a
 * 200 — that is what makes `/admin/users` work without a router on the server,
 * and it is not being changed here. The consequence is that a mistyped or
 * retired URL looks like a real page to a crawler: 200, HTML, a title.
 *
 * So the page says it instead. Googlebot renders the page before deciding, and
 * a `robots` meta it finds in the rendered DOM counts. This is a mitigation and
 * not a 404: the status code is still 200, and nothing in the front end can
 * change that. `robots.txt` covers the private paths as well, from the other
 * side.
 *
 * The canonical goes with it. The HTML the server sent for an unknown path is
 * the home page's, canonical included, so leaving it would pair "do not index"
 * with "this is really the home page" — and a `noindex` that points at another
 * URL can take that URL down with it. No canonical at all means the `noindex`
 * applies to this URL and to nothing else.
 */
export function markNoIndex(): void {
  if (typeof document === "undefined") return;
  document.querySelector('link[rel="canonical"]')?.remove();
  const existing = document.querySelector('meta[name="robots"]');
  if (existing) {
    existing.setAttribute("content", "noindex,follow");
    return;
  }
  const meta = document.createElement("meta");
  meta.setAttribute("name", "robots");
  meta.setAttribute("content", "noindex,follow");
  document.head.appendChild(meta);
}
