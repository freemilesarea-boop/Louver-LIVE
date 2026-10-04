/**
 * The service's name.
 *
 * Its own module so that the public marketing pages can use it without
 * importing `App.tsx` — which would pull the whole signed-in application into
 * the build-time prerender bundle. `App.tsx` re-exports it, so every existing
 * `import { Wordmark } from "../App"` keeps working.
 */
export function Wordmark({ className = "" }: { className?: string }) {
  return (
    <span className={`font-semibold tracking-tight ${className}`}>
      <span className="text-ink-100">247</span>
      <span className="text-ok">streams</span>
    </span>
  );
}
