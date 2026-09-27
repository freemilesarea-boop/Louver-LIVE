#!/usr/bin/env node
/**
 * The signup flow, in a real browser against a real server:
 * signup → auto-login → dashboard → logout → login again → dashboard.
 *
 * Separate from `browser-smoke.mjs`, which needs a video, an RTMP sink and
 * FFmpeg to prove a broadcast reaches a destination. This one proves only that a
 * person can make an account and get in, so it runs in seconds and needs
 * nothing but a server.
 *
 * Not part of `npm run verify`: it needs a browser binary and a running server,
 * neither of which belongs in a unit-test step.
 *
 *   LOUVER_DATA_DIR=$(mktemp -d) LOUVER_MASTER_KEY=$(openssl rand -hex 32) \
 *     LOUVER_WEB_DIR=apps/web/dist LOUVER_BIND=127.0.0.1:8099 \
 *     LOUVER_INSECURE_COOKIES=1 ./target/debug/louver-server &
 *   BASE=http://127.0.0.1:8099 node scripts/signup-smoke.mjs
 *
 * Always against a throwaway data directory. It creates an account, so pointing
 * it at production would put a test account in production.
 */
const { chromium } = await import("playwright");

const BASE = process.env.BASE ?? "http://127.0.0.1:8099";
const NAME = "홍길동";
const EMAIL = `smoke-${Date.now()}@example.com`;
const PASSWORD = "correct-horse-battery";
const log = (...a) => console.log("[smoke]", ...a);

const health = await (await fetch(`${BASE}/health`)).json();
log("server:", health.status, health.deployment);

const browser = await chromium.launch(
  process.env.CHROME_PATH ? { executablePath: process.env.CHROME_PATH } : {},
);
const ctx = await browser.newContext();
const page = await ctx.newPage();
const problems = [];
page.on("console", (m) => {
  // The app asks `/api/me` before drawing the form, so one 401 on first load is
  // the design rather than a fault.
  const expected401 = /401 \(Unauthorized\)/.test(m.text());
  if (m.type() === "error" && !expected401) problems.push(m.text());
  // A password reaching the console would be a leak whatever the level.
  if (m.text().includes(PASSWORD))
    problems.push(`PASSWORD IN CONSOLE: ${m.text()}`);
});

await page.goto(BASE);

// --- the login card is the simple one -------------------------------------
await page
  .getByRole("button", { name: "계정이 없으신가요? 회원가입" })
  .waitFor({ timeout: 10000 });
if (await page.getByLabel("이름").count())
  throw new Error("login mode must not ask for a name");
if (await page.getByLabel("비밀번호 확인").count())
  throw new Error("login mode must not confirm");
log("login card: email + password only ✓");

// --- signup ---------------------------------------------------------------
await page.getByRole("button", { name: "계정이 없으신가요? 회원가입" }).click();
await page.getByText("247streams 시작하기").waitFor();
await page
  .getByText("24시간 YouTube 라이브를 클라우드에서 운영하세요.")
  .waitFor();

// The terms link opens a page rather than a 404.
const terms = await ctx.newPage();
await terms.goto(`${BASE}/terms`);
const termsText = await terms.textContent("body");
if (!termsText.includes("이용약관")) throw new Error("/terms did not render");
if (!termsText.includes("준비 중인 초안"))
  throw new Error("/terms must say it is a draft");
await terms.goto(`${BASE}/privacy`);
if (!(await terms.textContent("body")).includes("개인정보처리방침"))
  throw new Error("/privacy did not render");
await terms.close();
log("/terms and /privacy render, and say they are drafts ✓");

// Mismatched confirmation: refused in the page, with no request.
let requests = 0;
page.on("request", (r) => {
  if (r.url().includes("/api/auth/register")) requests += 1;
});
await page.getByLabel("이름").fill(NAME);
await page.getByLabel("이메일").fill(EMAIL);
await page.getByLabel("비밀번호", { exact: true }).fill(PASSWORD);
await page.getByLabel("비밀번호 확인").fill("correct-horse-batter");
await page.getByLabel(/동의합니다/).check();
await page.getByRole("button", { name: "무료로 시작하기" }).click();
await page
  .getByRole("alert")
  .getByText("비밀번호가 일치하지 않습니다.")
  .waitFor({ timeout: 5000 });
if (requests !== 0)
  throw new Error("a mismatched password must not become a request");
log("mismatched password: refused locally, no request ✓");

// Now the real thing.
await page.getByLabel("비밀번호", { exact: true }).fill(PASSWORD);
await page.getByLabel("비밀번호 확인").fill(PASSWORD);
await page.getByRole("button", { name: "무료로 시작하기" }).click();
await page
  .getByRole("button", { name: "로그아웃" })
  .waitFor({ timeout: 15000 });
if (requests !== 1)
  throw new Error(`expected one register request, saw ${requests}`);
log("signed up and signed in automatically ✓");

// --- the dashboard, and the name in the header ----------------------------
const header = (await page.locator("header").first().textContent()).trim();
if (!header.includes(NAME))
  throw new Error(`the header should show ${NAME}: ${header}`);
if (header.includes(EMAIL))
  throw new Error("the email should give way to the name");
await page.getByRole("button", { name: "방송", exact: true }).waitFor();
await page.getByTestId("slots").waitFor({ timeout: 10000 });
log(
  "dashboard:",
  await page.getByTestId("slots").textContent(),
  "| header shows",
  NAME,
  "✓",
);

// --- the same email cannot be taken twice --------------------------------
const dup = await ctx.request.post(`${BASE}/api/auth/register`, {
  data: { name: "다른 사람", email: EMAIL.toUpperCase(), password: PASSWORD },
});
if (dup.status() !== 409)
  throw new Error(`duplicate email should be 409, got ${dup.status()}`);
const dupBody = await dup.text();
if (!dupBody.includes("이메일"))
  throw new Error(`unhelpful duplicate message: ${dupBody}`);
if (/UNIQUE|sqlite|constraint/i.test(dupBody))
  throw new Error(`database internals leaked: ${dupBody}`);
log("duplicate email → 409,", JSON.parse(dupBody).error, "✓");

// --- log out, and back in -------------------------------------------------
await page.getByRole("button", { name: "로그아웃" }).click();
await page
  .getByRole("button", { name: "계정이 없으신가요? 회원가입" })
  .waitFor({ timeout: 10000 });
await page.getByLabel("이메일").fill(EMAIL);
await page.getByLabel("비밀번호", { exact: true }).fill(PASSWORD);
await page.getByRole("button", { name: "로그인" }).click();
await page
  .getByRole("button", { name: "로그아웃" })
  .waitFor({ timeout: 15000 });
if (!(await page.locator("header").first().textContent()).includes(NAME))
  throw new Error("name lost after re-login");
await page.getByTestId("slots").waitFor({ timeout: 10000 });
log("logged out and back in with the new account ✓");

// --- nothing was written where a script could read it --------------------
const stored = await page.evaluate(() => ({
  local: JSON.stringify(localStorage),
  session: JSON.stringify(sessionStorage),
  cookie: document.cookie,
}));
for (const [where, text] of Object.entries(stored)) {
  if (text.includes(PASSWORD)) throw new Error(`the password is in ${where}`);
  if (/louver_session/.test(text))
    throw new Error(`the session token is readable in ${where}`);
}
log(
  "no password and no token in localStorage/sessionStorage/document.cookie ✓",
);

if (problems.length) {
  console.error("[smoke] console problems:", problems);
  process.exit(1);
}
await browser.close();
log("ALL PASS");
