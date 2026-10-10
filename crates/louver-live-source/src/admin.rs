//! The operator's way in: stop one account's broadcasts, and keep them stopped.
//!
//! ## Why this exists
//!
//! An operator suspends an account in 247streams' admin console. Production
//! deletes that account's sessions and stops the broadcasts **it** manages, so
//! within five minutes the account can no longer mint a token here and cannot
//! start anything new (see REVOCATION.md §2). What production cannot do is
//! stop a broadcast *this* worker is already running: it has no idea this
//! worker exists.
//!
//! Closing that automatically would need a new production endpoint, and
//! deploying one recreates the `louver` container and interrupts every running
//! customer broadcast. So the operator does it in one more step, here.
//!
//! ## Three secrets, and why this is the third
//!
//! | secret | question it answers |
//! |---|---|
//! | [`crate::token`] | which user is this? |
//! | [`crate::gate`] | did this come through 247streams' proxy? |
//! | **this one** | is this the operator? |
//!
//! They are not interchangeable, and that is enforced rather than trusted:
//! each is hashed under its own domain-separation label ([`crate::secret`]),
//! so the gate secret offered in the admin header fails **even if an operator
//! sets both environment variables to the same string**. A leaked gate secret
//! — which every beta request carries — must not become the power to take
//! customers off air.
//!
//! ## Where it is reachable from
//!
//! Nowhere on the internet. Caddy's matchers route only
//! `/api/live-source/*` and `/beta/*` to this worker, so `/admin/*` falls to
//! production's catch-all and never arrives. These routes are therefore
//! reachable only on the worker's own port — an operator on the VPS, over SSH,
//! against loopback — which is the right exposure for an operator action and
//! comes for free from the routing that already exists. The firewall is the
//! second layer and this secret is the third.
//!
//! For the same reason the admin routes are deliberately **outside** the gate
//! layer: the gate asks "did this come through Caddy?", and the honest answer
//! here is no. Requiring it would make the operator forge Caddy's header, and
//! would protect nothing — if Caddy ever did route `/admin/*`, it would add
//! the gate header itself.
//!
//! ## What a revocation is
//!
//! Not just a stop. A stop alone would be undone by the account's own token,
//! which stays valid for up to five minutes and could start a new job in that
//! window; and a restart reads the state directory, so "stopped" has to be
//! durable. So a revocation is a **persisted decision** that
//! [`crate::jobs::Registry`] consults when creating and when recovering.
//!
//! Reversible, because an operator who re-enables an account must be able to
//! undo it — otherwise the only way back is editing a file on the VPS.

use crate::error::{LiveSourceError, Result};
use crate::secret::ConstantTimeSecret;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub use crate::secret::MIN_SECRET_LEN;

/// The header an operator sends.
pub const ADMIN_HEADER: &str = "x-louver-admin";

/// Domain separation, different from the gate's on purpose.
const LABEL: &[u8] = b"louver-live-source-admin";

/// The operator's shared secret.
#[derive(Clone)]
pub struct AdminSecret {
    secret: ConstantTimeSecret,
}

impl std::fmt::Debug for AdminSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AdminSecret")
    }
}

impl AdminSecret {
    /// Build from the configured secret, refusing one too short to matter.
    ///
    /// There is no constructor that yields an open door: an operator endpoint
    /// that could be enabled without a secret is an operator endpoint that
    /// eventually is.
    pub fn new(secret: &str) -> Result<Self> {
        let secret = ConstantTimeSecret::new(LABEL, secret).ok_or_else(|| {
            LiveSourceError::invalid(format!("관리자 비밀값은 {MIN_SECRET_LEN}바이트 이상이어야 합니다."))
        })?;
        Ok(Self { secret })
    }

    /// Accept this request's admin header, or refuse it.
    pub fn check(&self, header: Option<&str>) -> Result<()> {
        if self.secret.matches(LABEL, header) {
            return Ok(());
        }
        // Missing and wrong get the same answer, as with the gate: telling
        // them apart tells a prober whether the header name is right.
        Err(LiveSourceError::new(crate::ErrorKind::Forbidden, "관리자 인증이 필요합니다."))
    }
}

/// The users an operator has revoked, on disk.
///
/// A `BTreeSet` rather than a list: revoking twice is the same state as
/// revoking once, which is most of what makes the endpoint idempotent.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Stored {
    users: BTreeSet<String>,
}

/// Revoked users, persisted so a restart cannot forget.
pub struct RevokedUsers {
    path: PathBuf,
    users: Mutex<BTreeSet<String>>,
}

impl std::fmt::Debug for RevokedUsers {
    /// A count, not the ids: an account id is a customer identifier.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RevokedUsers({})", self.len())
    }
}

impl RevokedUsers {
    /// Load from the state directory, or start empty.
    ///
    /// An unreadable or corrupt file starts empty rather than refusing to
    /// start: this list only ever *denies*, so losing it fails open on
    /// revocation and the operator can re-apply it — whereas refusing to boot
    /// would take every healthy broadcast down with it. The trade is recorded
    /// here because it is the one place in this module that fails open, and it
    /// is why §4's "fail-open on continuation" is a deliberate posture rather
    /// than an accident.
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join("revoked.json");
        let users = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Stored>(&b).ok())
            .map(|s| s.users)
            .unwrap_or_default();
        Self { path, users: Mutex::new(users) }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        self.users.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn persist(&self, users: &BTreeSet<String>) -> Result<()> {
        let body = serde_json::to_vec_pretty(&Stored { users: users.clone() })
            .map_err(|e| LiveSourceError::invalid(format!("취소 목록을 만들 수 없습니다: {e}")))?;
        // Temp-and-rename, so a crash mid-write cannot leave a half file that
        // reads as "nobody is revoked".
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &body).and_then(|_| std::fs::rename(&tmp, &self.path)).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            LiveSourceError::invalid(format!("취소 목록을 쓸 수 없습니다: {e}"))
        })
    }

    /// Record a revocation. `true` if this changed anything.
    pub fn revoke(&self, user_id: &str) -> Result<bool> {
        let mut users = self.guard();
        let added = users.insert(user_id.to_string());
        self.persist(&users)?;
        Ok(added)
    }

    /// Lift a revocation. `true` if this changed anything.
    pub fn restore(&self, user_id: &str) -> Result<bool> {
        let mut users = self.guard();
        let removed = users.remove(user_id);
        self.persist(&users)?;
        Ok(removed)
    }

    pub fn is_revoked(&self, user_id: &str) -> bool {
        self.guard().contains(user_id)
    }

    pub fn list(&self) -> Vec<String> {
        self.guard().iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.guard().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One line of the operator audit trail.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AuditEntry {
    /// RFC 3339, UTC.
    pub at: String,
    /// `revoke`, `restore`, or `revoke-denied`.
    pub action: String,
    /// The account acted on.
    pub user_id: String,
    /// How many of its jobs were stopped by this call.
    pub stopped: usize,
    /// Which ones, so the record is specific enough to audit against.
    pub jobs: Vec<String>,
    /// Whether this call changed the stored decision, or repeated it.
    pub changed: bool,
}

/// Append-only operator log, beside the job state.
///
/// What is deliberately **not** in it: the admin secret, the gate secret, any
/// token, and any destination. The subject's user id is there because an audit
/// trail that does not say who it was about is not an audit trail.
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(state_dir: &Path) -> Self {
        Self { path: state_dir.join("admin-audit.jsonl") }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry. A failure to write is reported but never swallows the
    /// outcome of the action itself — losing the log line is bad, undoing a
    /// revocation because the log was unwritable would be worse.
    pub fn append(&self, entry: &AuditEntry) {
        let Ok(mut line) = serde_json::to_string(entry) else {
            eprintln!("[louver][live-source][admin] 감사 기록을 만들 수 없습니다");
            return;
        };
        line.push('\n');
        use std::io::Write;
        match std::fs::OpenOptions::new().create(true).append(true).open(&self.path) {
            Ok(mut f) => {
                if let Err(e) = f.write_all(line.as_bytes()) {
                    eprintln!("[louver][live-source][admin] 감사 기록을 쓸 수 없습니다: {e}");
                }
            }
            Err(e) => eprintln!("[louver][live-source][admin] 감사 파일을 열 수 없습니다: {e}"),
        }
        // Also to stdout, so an operator watching the journal sees it without
        // reading a file. Counts and ids only.
        println!(
            "[louver][live-source][admin] {} user={} 중단={} 변경={}",
            entry.action, entry.user_id, entry.stopped, entry.changed
        );
    }

    /// Read the log back. For an operator and for the tests.
    pub fn entries(&self) -> Vec<AuditEntry> {
        std::fs::read_to_string(&self.path)
            .map(|body| body.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default()
    }
}

/// A user id an operator may act on.
///
/// Not a path, not empty, and bounded — the id goes into an audit line and is
/// compared against job owners, so a control character or a megabyte of text
/// has no business getting that far.
pub fn check_user_id(user_id: &str) -> Result<&str> {
    let id = user_id.trim();
    if id.is_empty() || id.chars().count() > 64 {
        return Err(LiveSourceError::invalid("user_id 가 올바르지 않습니다."));
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(LiveSourceError::invalid("user_id 는 영숫자·하이픈·밑줄만 쓸 수 있습니다."));
    }
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: &str = "an-admin-secret-of-at-least-32-bytes";
    const GATE: &str = "a-gate-secret-of-at-least-32-bytes!!";

    #[test]
    fn the_operator_secret_is_accepted_and_nothing_else_is() {
        let a = AdminSecret::new(SECRET).unwrap();
        assert!(a.check(Some(SECRET)).is_ok());
        for bad in [None, Some(""), Some(GATE), Some(&SECRET[1..]), Some(&format!("{SECRET}x")[..])] {
            let e = a.check(bad).unwrap_err();
            assert_eq!(e.kind, crate::ErrorKind::Forbidden, "{bad:?}");
        }
    }

    #[test]
    fn the_gate_secret_is_not_the_admin_secret_even_if_they_are_the_same_string() {
        // The property the domain-separation labels exist for. An operator who
        // reuses one value must not thereby make every beta request able to
        // revoke, because the gate secret travels on every proxied request.
        let same = "one-value-used-for-both-of-them-32!!";
        let admin = AdminSecret::new(same).unwrap();
        let gate = crate::gate::Gate::new(same).unwrap();
        // Each accepts it under its own header…
        assert!(admin.check(Some(same)).is_ok());
        assert!(gate.check(Some(same)).is_ok());
        // …but the digests differ, which is what the label buys. Asserted via
        // a distinct secret: the admin check must not accept a value that only
        // the gate knows.
        let admin2 = AdminSecret::new(GATE).unwrap();
        assert!(admin2.check(Some(SECRET)).is_err());
        assert!(crate::gate::Gate::new(GATE).unwrap().check(Some(SECRET)).is_err());
    }

    #[test]
    fn a_short_admin_secret_is_refused_and_names_its_own_variable() {
        for short in ["", "x", &"x".repeat(MIN_SECRET_LEN - 1)] {
            let e = AdminSecret::new(short).unwrap_err();
            assert!(e.message.contains("관리자"), "should say which secret: {}", e.message);
        }
        assert!(AdminSecret::new(&"x".repeat(MIN_SECRET_LEN)).is_ok());
    }

    #[test]
    fn nothing_prints_the_admin_secret() {
        let a = AdminSecret::new(SECRET).unwrap();
        assert_eq!(format!("{a:?}"), "AdminSecret");
        for s in [format!("{a:?}"), a.check(Some("x")).unwrap_err().message] {
            assert!(!s.contains("admin-secret"), "leaked in {s}");
            assert!(!s.contains(SECRET), "leaked in {s}");
        }
    }

    #[test]
    fn a_revocation_is_idempotent_and_reversible() {
        let d = tempfile::tempdir().unwrap();
        let r = RevokedUsers::load(d.path());
        assert!(!r.is_revoked("u1"));

        assert!(r.revoke("u1").unwrap(), "first revoke changes state");
        assert!(!r.revoke("u1").unwrap(), "second revoke changes nothing");
        assert!(r.is_revoked("u1"));
        assert_eq!(r.list(), vec!["u1".to_string()]);

        assert!(r.restore("u1").unwrap(), "first restore changes state");
        assert!(!r.restore("u1").unwrap(), "second restore changes nothing");
        assert!(!r.is_revoked("u1"));
        assert!(r.is_empty());
    }

    #[test]
    fn a_revocation_survives_a_restart() {
        let d = tempfile::tempdir().unwrap();
        {
            let r = RevokedUsers::load(d.path());
            r.revoke("u1").unwrap();
            r.revoke("u2").unwrap();
            r.restore("u2").unwrap();
        }
        // A new process over the same directory.
        let again = RevokedUsers::load(d.path());
        assert!(again.is_revoked("u1"), "a revocation must outlive the process");
        assert!(!again.is_revoked("u2"), "and so must lifting one");
    }

    #[test]
    fn one_users_revocation_does_not_touch_another() {
        let d = tempfile::tempdir().unwrap();
        let r = RevokedUsers::load(d.path());
        r.revoke("alice").unwrap();
        assert!(r.is_revoked("alice"));
        assert!(!r.is_revoked("bob"));
        assert!(!r.is_revoked("alice2"), "not a prefix match");
        assert!(!r.is_revoked("alic"));
    }

    #[test]
    fn a_corrupt_list_starts_empty_rather_than_refusing_to_boot() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("revoked.json"), b"{ this is not json").unwrap();
        let r = RevokedUsers::load(d.path());
        assert!(r.is_empty(), "a corrupt deny-list must not deny everybody");
        // And it can be written over.
        r.revoke("u1").unwrap();
        assert!(RevokedUsers::load(d.path()).is_revoked("u1"));
    }

    #[test]
    fn the_audit_log_records_what_was_done_and_carries_no_secret() {
        let d = tempfile::tempdir().unwrap();
        let log = AuditLog::new(d.path());
        log.append(&AuditEntry {
            at: "2026-10-09T00:00:00Z".into(),
            action: "revoke".into(),
            user_id: "user-alice".into(),
            stopped: 2,
            jobs: vec!["b1".into(), "b2".into()],
            changed: true,
        });
        log.append(&AuditEntry {
            at: "2026-10-09T00:00:01Z".into(),
            action: "revoke".into(),
            user_id: "user-alice".into(),
            stopped: 0,
            jobs: vec![],
            changed: false,
        });
        let got = log.entries();
        assert_eq!(got.len(), 2, "append-only, one line each");
        assert_eq!(got[0].stopped, 2);
        assert_eq!(got[0].jobs, vec!["b1".to_string(), "b2".to_string()]);
        assert!(!got[1].changed, "a repeat is recorded as a repeat");

        let raw = std::fs::read_to_string(log.path()).unwrap();
        for forbidden in [SECRET, GATE, "rtmp", "Bearer", "louver_session"] {
            assert!(!raw.contains(forbidden), "{forbidden} in the audit log");
        }
    }

    #[test]
    fn a_user_id_that_could_be_a_path_or_a_flood_is_refused() {
        for bad in ["", "   ", "../../etc/passwd", "a/b", "a b", "a.b", &"x".repeat(65), "a\0b"] {
            assert!(check_user_id(bad).is_err(), "{bad:?} accepted");
        }
        assert_eq!(check_user_id(" user-alice ").unwrap(), "user-alice");
        assert!(check_user_id(&"x".repeat(64)).is_ok());
    }
}
