// A stand-in for `louver-server` in the front-end split check.
//
// It exists so that `scripts/web-split-check.sh` can exercise the real
// `deploy/web/Caddyfile` against something that behaves like the application —
// a JSON route, the one non-/api route (`/health`), a session cookie and a
// server-sent-events stream that stays open — without building the Rust server
// and, above all, without going anywhere near the running one.
//
//   node scripts/web-split-stub-api.mjs --port 9703
import http from 'node:http'

const args = process.argv.slice(2)
const port = Number(args[args.indexOf('--port') + 1] || 9703)

const json = (res, code, body) => {
  const text = JSON.stringify(body)
  res.writeHead(code, { 'Content-Type': 'application/json', 'Content-Length': Buffer.byteLength(text) })
  res.end(text)
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, 'http://stub')
  if (url.pathname === '/api/events') {
    // The dashboard's live updates. Written unbuffered and left open, which is
    // the property the proxy configuration has to preserve.
    res.writeHead(200, {
      'Content-Type': 'text/event-stream',
      'Cache-Control': 'no-cache',
      Connection: 'keep-alive',
    })
    let n = 0
    const timer = setInterval(() => {
      n += 1
      res.write(`data: tick ${n}\n\n`)
    }, 200)
    req.on('close', () => clearInterval(timer))
    return
  }
  if (url.pathname === '/health' || url.pathname === '/api/health') {
    return json(res, 200, { ok: true, from: 'stub-api', path: url.pathname })
  }
  if (url.pathname === '/api/me') {
    // Enough to tell a proxied request that carried its cookie from one that
    // lost it on the way through.
    res.setHeader('Set-Cookie', 'louver_session=stub; HttpOnly; Path=/; SameSite=Lax')
    return json(res, req.headers.cookie ? 200 : 401, {
      cookie_seen: req.headers.cookie ?? null,
      from: 'stub-api',
    })
  }
  return json(res, 404, { error: 'no such route', from: 'stub-api' })
})

server.listen(port, '127.0.0.1', () => console.log(`stub api on 127.0.0.1:${port}`))
