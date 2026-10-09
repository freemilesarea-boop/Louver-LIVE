"use strict";
/* 247streams — YouTube Live 영상 소스 베타 페이지, 동작
 *
 * ## The token
 *
 * It is held in this closure and nowhere else — not in localStorage, not in a
 * cookie this page sets. It dies with the tab.
 *
 * It is valid for five minutes, which is short enough that the renewal has to
 * be reliable rather than best-effort. Three things make it so:
 *
 *  * a timer that renews at roughly 60% of the lifetime, so a renewal that
 *    fails still has time to be retried before the token dies;
 *  * `fresh()` before every call, so a tab that was asleep (and whose timer
 *    therefore did not fire on time) renews on its next action instead of
 *    sending a token it can already tell is expired;
 *  * one retry on a 401, so a token that expired between the check and the
 *    server reading it is renewed and the request is repeated rather than
 *    surfacing as an error the user has to understand.
 *
 * Renewal needs the 247streams session cookie, which the browser attaches to
 * `POST …/session` on its own. So a renewal that comes back 401 means the
 * session itself is gone — logged out, or expired — and the page says so and
 * stops rather than retrying forever.
 */

const API = "/api/live-source";

let token = null;
/** Epoch ms when the token stops being usable, as this page understands it. */
let tokenDeadline = 0;
let renewTimer = null;
/** The renewal in flight, so a timer and a click do not start two. */
let renewing = null;
let poll = null;
let destinationNames = [];
let limits = { maxUpload: 0, maxStorage: 0, extensions: [] };

const $ = (id) => document.getElementById(id);
const say = (el, text, kind) => {
  el.textContent = text;
  el.className = "msg" + (kind ? " " + kind : "");
};
const mb = (n) => Math.round(n / 1024 / 1024).toLocaleString() + "MB";

/* ------------------------------------------------------------------ token -- */

/** The handshake. The only request that uses the session cookie. */
async function handshake() {
  const res = await fetch(API + "/session", {
    method: "POST",
    // Same origin, so the session cookie rides along and nothing needs CORS.
    // `same-origin` and not `include`: this page has no business sending
    // credentials anywhere else.
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  });
  let data = null;
  try { data = await res.json(); } catch (_) { /* a body is not guaranteed */ }
  if (!res.ok) {
    const err = new Error((data && data.message) || ("로그인 확인이 실패했습니다 (HTTP " + res.status + ")"));
    err.status = res.status;
    throw err;
  }
  token = data.token;
  // Trimmed by five seconds so this page's idea of "expired" is always a
  // little earlier than the server's, never later.
  tokenDeadline = Date.now() + Math.max(5, data.expires_in - 5) * 1000;

  destinationNames = data.destinations || [];
  limits = {
    maxUpload: data.max_upload_bytes || 0,
    maxStorage: data.max_storage_bytes || 0,
    extensions: data.allowed_extensions || [],
  };

  $("cap").textContent = data.max_height + "p";
  const dest = $("dest");
  const chosen = dest.value;
  dest.textContent = "";
  for (const name of destinationNames) {
    const o = document.createElement("option");
    o.value = name;
    o.textContent = name;
    dest.appendChild(o);
  }
  if (destinationNames.includes(chosen)) dest.value = chosen;
  $("mediaHint").textContent =
    "· 한 파일 최대 " + mb(limits.maxUpload) + " · " + limits.extensions.join(", ");

  // Renew at about 60% of the lifetime: early enough that a failed renewal can
  // be tried again before the token is actually gone.
  const after = Math.max(20, Math.round(data.expires_in * 0.6));
  if (renewTimer) clearTimeout(renewTimer);
  renewTimer = setTimeout(() => { renew().catch(() => {}); }, after * 1000);
  return data;
}

/** One renewal at a time, whoever asked for it. */
function renew() {
  if (!renewing) {
    renewing = handshake().finally(() => { renewing = null; });
  }
  return renewing;
}

/** A usable token, renewing first if this page can already tell it is stale. */
async function fresh() {
  if (!token || Date.now() >= tokenDeadline) await renew();
  return token;
}

/* ------------------------------------------------------------------- call -- */

async function call(path, opts = {}) {
  await fresh();
  const send = async () => {
    const headers = Object.assign({ Accept: "application/json" }, opts.headers || {});
    if (opts.body !== undefined) headers["Content-Type"] = opts.json === false ? "application/octet-stream" : "application/json";
    headers["Authorization"] = "Bearer " + token;
    return fetch(API + path, {
      method: opts.method || "GET",
      // No credentials on these: the API refuses a Cookie header anywhere but
      // the handshake, so sending one would be a 403 rather than a convenience.
      credentials: "omit",
      headers,
      body: opts.body === undefined ? undefined : (opts.json === false ? opts.body : JSON.stringify(opts.body)),
    });
  };

  let res = await send();
  // The token expired between `fresh()` and the server reading it. Renew once
  // and repeat — a second 401 is a real one.
  if (res.status === 401) {
    token = null;
    await renew();
    res = await send();
  }
  let data = null;
  try { data = await res.json(); } catch (_) { /* a body is not guaranteed */ }
  if (!res.ok) {
    const err = new Error((data && data.message) || ("요청이 실패했습니다 (HTTP " + res.status + ")"));
    err.status = res.status;
    err.code = data && data.error;
    throw err;
  }
  return data;
}

/* --------------------------------------------------------------- connect -- */

function disconnected(message) {
  token = null;
  tokenDeadline = 0;
  if (renewTimer) { clearTimeout(renewTimer); renewTimer = null; }
  if (poll) { clearInterval(poll); poll = null; }
  $("newCard").hidden = true;
  $("mediaCard").hidden = true;
  say($("authMsg"), message, "bad");
}

async function connect() {
  $("authBtn").disabled = true;
  say($("authMsg"), "확인 중…", null);
  try {
    const s = await renew();
    say($("authMsg"),
      "연결됨 · 내 동시 방송 " + s.max_per_user + "개까지 (서버 전체 " + s.max_concurrent + "개) · 토큰 "
        + Math.round(s.expires_in / 60) + "분, 자동 갱신",
      "ok");
    $("newCard").hidden = false;
    $("mediaCard").hidden = false;
    $("jobsCard").hidden = false;
    if (destinationNames.length === 0) {
      say($("createMsg"), "이 계정에 등록된 송출 대상이 없습니다. 운영자에게 요청해 주세요.", "warn");
    }
    await Promise.all([refresh(), refreshMedia()]);
    if (!poll) poll = setInterval(() => { refresh().catch(() => {}); }, 3000);
  } catch (e) {
    disconnected(e.status === 401
      ? "247streams에 로그인한 뒤 다시 확인해 주세요. (로그아웃되었거나 세션이 만료되었습니다)"
      : e.message);
  } finally {
    $("authBtn").disabled = false;
  }
}

/* ----------------------------------------------------------------- media -- */

async function refreshMedia() {
  let data;
  try { data = await call("/media"); }
  catch (e) { say($("upMsg"), e.message, "bad"); return; }

  $("mediaUsage").textContent =
    "· " + mb(data.used_bytes) + " / " + mb(data.max_storage_bytes);

  const list = $("mediaList");
  list.textContent = "";
  if (data.media.length === 0) {
    list.className = "empty";
    list.textContent = "아직 올린 파일이 없습니다.";
    return;
  }
  list.className = "";
  const table = document.createElement("table");
  const tbody = document.createElement("tbody");
  for (const m of data.media) {
    const tr = document.createElement("tr");
    const nameTd = document.createElement("td");
    const c = document.createElement("code");
    // textContent, never innerHTML: a file name is a user-supplied string and
    // this page will not render one as markup.
    c.textContent = m.name;
    nameTd.appendChild(c);
    tr.appendChild(nameTd);

    const sizeTd = document.createElement("td");
    sizeTd.textContent = mb(m.bytes);
    tr.appendChild(sizeTd);

    const actTd = document.createElement("td");
    const b = document.createElement("button");
    b.className = "ghost small";
    b.textContent = "삭제";
    b.addEventListener("click", () => remove(m.name, b));
    actTd.appendChild(b);
    tr.appendChild(actTd);
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  list.appendChild(table);
}

async function remove(name, btn) {
  btn.disabled = true;
  try {
    await call("/media/" + encodeURIComponent(name), { method: "DELETE" });
    say($("upMsg"), name + " 삭제됨", "ok");
  } catch (e) {
    say($("upMsg"), e.message, "bad");
  } finally {
    btn.disabled = false;
    refreshMedia();
  }
}

async function upload() {
  const files = Array.from($("file").files || []);
  if (files.length === 0) return say($("upMsg"), "올릴 파일을 선택해 주세요.", "warn");
  $("upBtn").disabled = true;
  $("upBar").hidden = false;

  let done = 0;
  for (const f of files) {
    say($("upMsg"), "올리는 중… " + f.name + " (" + (done + 1) + "/" + files.length + ")", null);
    $("upFill").style.width = Math.round((done / files.length) * 100) + "%";
    if (limits.maxUpload && f.size > limits.maxUpload) {
      say($("upMsg"), f.name + ": 파일이 너무 큽니다 (최대 " + mb(limits.maxUpload) + ")", "bad");
      break;
    }
    try {
      // The name goes in the path and the bytes are the body: there is no
      // multipart form and no field a client could use to name a path.
      await call("/media/" + encodeURIComponent(f.name), { method: "PUT", body: f, json: false });
      done += 1;
    } catch (e) {
      say($("upMsg"), f.name + ": " + e.message, "bad");
      break;
    }
  }
  $("upFill").style.width = Math.round((done / files.length) * 100) + "%";
  if (done === files.length) say($("upMsg"), done + "개 올렸습니다.", "ok");
  $("file").value = "";
  $("upBtn").disabled = false;
  setTimeout(() => { $("upBar").hidden = true; $("upFill").style.width = "0"; }, 1200);
  refreshMedia();
}

/* ------------------------------------------------------------------ jobs -- */

async function check() {
  const url = $("src").value.trim();
  if (!url) return say($("checkMsg"), "주소를 입력해 주세요.", "warn");
  $("checkBtn").disabled = true;
  say($("checkMsg"), "확인 중…", null);
  try {
    const r = await call("/check", { method: "POST", body: { source_url: url } });
    say($("checkMsg"), (r.ok ? "OK · " : "실패 · ") + r.message, r.ok ? "ok" : "bad");
  } catch (e) {
    say($("checkMsg"), e.message, "bad");
  } finally {
    $("checkBtn").disabled = false;
  }
}

async function create() {
  const playlist = $("pl").value.split("\n").map((s) => s.trim()).filter(Boolean);
  $("createBtn").disabled = true;
  say($("createMsg"), "시작 중…", null);
  try {
    const j = await call("/jobs", {
      method: "POST",
      body: {
        broadcast_id: $("bid").value.trim(),
        source_url: $("src").value.trim(),
        playlist,
        destination: $("dest").value,
      },
    });
    say($("createMsg"), "시작됨 · " + j.broadcast_id, "ok");
    refresh();
  } catch (e) {
    say($("createMsg"), e.message, "bad");
  } finally {
    $("createBtn").disabled = false;
  }
}

async function cancel(id, btn) {
  btn.disabled = true;
  try { await call("/jobs/" + encodeURIComponent(id), { method: "DELETE" }); }
  catch (e) { say($("createMsg"), e.message, "bad"); }
  finally { btn.disabled = false; refresh(); }
}

const PHASE_KO = {
  resolving: "소스 확인 중", starting: "시작 중", sending: "송출 중",
  reconnecting: "재연결 중", gave_up: "중단됨", stopped: "정지",
};

async function refresh() {
  let data;
  try {
    data = await call("/jobs");
  } catch (e) {
    // A 401 that survived the retry inside `call` means the session is gone.
    if (e.status === 401) {
      disconnected("247streams 세션이 만료되었거나 로그아웃되었습니다. 다시 확인해 주세요.");
    }
    return;
  }

  $("counts").textContent =
    "· 내 방송 " + data.running_mine + " / " + data.max_per_user
    + " · 서버 전체 " + data.running + " / " + data.max_concurrent;
  const body = $("jobsBody");
  body.textContent = "";
  $("jobsEmpty").hidden = data.jobs.length > 0;

  for (const j of data.jobs) {
    const tr = document.createElement("tr");
    const cell = (text) => { const td = document.createElement("td"); td.textContent = text; return td; };

    // textContent everywhere, never innerHTML: a broadcast id and an error
    // message are user-influenced strings, and this page will not render them
    // as markup.
    const idTd = document.createElement("td");
    const code = document.createElement("code");
    code.textContent = j.broadcast_id;
    idTd.appendChild(code);
    tr.appendChild(idTd);

    const stTd = document.createElement("td");
    const pill = document.createElement("span");
    pill.className = "pill " + j.phase;
    pill.textContent = PHASE_KO[j.phase] || j.phase;
    stTd.appendChild(pill);
    if (j.last_error) {
      const d = document.createElement("div");
      d.className = "note";
      d.textContent = j.last_error;
      stTd.appendChild(d);
    }
    tr.appendChild(stTd);

    tr.appendChild(cell(j.frames.toLocaleString()));
    tr.appendChild(cell(j.restarts + (j.last_verdict && j.last_verdict !== "healthy" ? " (" + j.last_verdict + ")" : "")));
    tr.appendChild(cell(j.video_id || "—"));

    const actTd = document.createElement("td");
    if (j.desired === "running") {
      const b = document.createElement("button");
      b.className = "ghost small";
      b.textContent = "정지";
      b.addEventListener("click", () => cancel(j.broadcast_id, b));
      actTd.appendChild(b);
    }
    tr.appendChild(actTd);
    body.appendChild(tr);
  }
}

/* ------------------------------------------------------------------ start -- */

$("authBtn").addEventListener("click", connect);
$("checkBtn").addEventListener("click", check);
$("createBtn").addEventListener("click", create);
$("upBtn").addEventListener("click", upload);
// A tab that was asleep comes back with a token the timer never renewed. This
// is what makes the five-minute lifetime invisible to someone switching tabs.
document.addEventListener("visibilitychange", () => {
  if (!document.hidden && token && Date.now() >= tokenDeadline) renew().catch(() => {});
});
connect();
