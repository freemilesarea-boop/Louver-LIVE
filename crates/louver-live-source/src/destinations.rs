//! Where a user may send, kept apart per user.
//!
//! An RTMP(S) URL carries the stream key, so this module holds the one thing in
//! the service that is unambiguously a credential. Three rules follow from
//! that, and they are the whole design:
//!
//!  1. **A client never sends a URL.** It names a destination, and the name is
//!     looked up here. A client that could post a URL could point somebody
//!     else's picture and music at its own ingest.
//!  2. **A user sees only their own names.** Not the URLs — the *names*. Even a
//!     name is a leak: "acme-main" tells one customer that another exists, and
//!     a name is enough to try sending to it. So the lookup takes the user id
//!     and a name belonging to anyone else reads as *not registered*, exactly
//!     as a name nobody registered does.
//!  3. **Nothing hands out the map.** There is no accessor that returns a URL
//!     to a caller, no `Debug` that prints one, and no error message that
//!     quotes one.
//!
//! Configuration shape, from the environment only:
//!
//! ```json
//! { "user-id-1": { "my-channel": "rtmps://a.rtmps.youtube.com/live2/KEY" },
//!   "user-id-2": { "test-sink":  "rtmp://127.0.0.1:1935/live/test" } }
//! ```
//!
//! Earlier this was one flat `{name: url}` map shared by everybody, which meant
//! every beta user could see and use every other user's destination. That is
//! the defect this module exists to close.

use crate::error::{LiveSourceError, Result};
use std::collections::BTreeMap;

/// `user_id` → (`name` → url).
///
/// Deliberately **not** `Deserialize`. A derive here would be a second way in
/// that skipped [`Destinations::check`], and the whole value of validating at
/// startup is that there is no unvalidated path. The two constructors are
/// [`Destinations::parse`] and [`Destinations::from_map`], and both check.
#[derive(Clone, Default)]
pub struct Destinations {
    by_user: BTreeMap<String, BTreeMap<String, String>>,
}

impl std::fmt::Debug for Destinations {
    /// Counts, never contents. A `{:?}` of this must be safe to paste into a
    /// support thread.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Destinations({} users)", self.by_user.len())
    }
}

impl Destinations {
    /// Read the configured map, refusing anything that is not a usable ingest.
    ///
    /// Validated at startup rather than on first use, so a typo is a refusal to
    /// start instead of a broadcast that fails at air time.
    pub fn parse(raw: &str) -> Result<Self> {
        // The error is deliberately not quoted: a malformed value may still
        // contain a stream key, and serde's message echoes the input.
        let by_user: BTreeMap<String, BTreeMap<String, String>> = serde_json::from_str(raw)
            .map_err(|_| LiveSourceError::invalid("송출 대상 설정을 JSON 객체로 해석할 수 없습니다."))?;
        let me = Self { by_user };
        me.check()?;
        Ok(me)
    }

    fn check(&self) -> Result<()> {
        if self.by_user.is_empty() {
            return Err(LiveSourceError::invalid("송출 대상이 하나도 없습니다."));
        }
        for (user, names) in &self.by_user {
            if user.trim().is_empty() {
                return Err(LiveSourceError::invalid("송출 대상 설정에 사용자 id 가 비어 있습니다."));
            }
            if names.is_empty() {
                // An empty group is almost certainly a mistake, and a silent one.
                return Err(LiveSourceError::invalid("송출 대상이 없는 사용자 항목이 있습니다."));
            }
            for (name, url) in names {
                if !name_ok(name) {
                    return Err(LiveSourceError::invalid(
                        "송출 대상 이름은 영숫자·하이픈·밑줄 1~40자만 쓸 수 있습니다.",
                    ));
                }
                // Only an ingest. `file:`, `http:` and a shell-shaped string are
                // all refused here rather than discovered by FFmpeg.
                if !(url.starts_with("rtmp://") || url.starts_with("rtmps://")) {
                    return Err(LiveSourceError::invalid(
                        "송출 대상은 rtmp:// 또는 rtmps:// 주소여야 합니다.",
                    ));
                }
                if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
                    return Err(LiveSourceError::invalid("송출 대상 주소에 공백이나 제어문자가 있습니다."));
                }
            }
        }
        Ok(())
    }

    /// Build one directly. For tests and for a caller that already has a map.
    pub fn from_map(by_user: BTreeMap<String, BTreeMap<String, String>>) -> Result<Self> {
        let me = Self { by_user };
        me.check()?;
        Ok(me)
    }

    /// The names this user may choose. Never anyone else's, never a URL.
    pub fn names_for(&self, user_id: &str) -> Vec<String> {
        self.by_user.get(user_id).map(|m| m.keys().cloned().collect()).unwrap_or_default()
    }

    /// This user's destination, by their name for it.
    ///
    /// Returns the same refusal for "no such name", "that is another user's
    /// name" and "you have no destinations at all". The three are
    /// indistinguishable to the caller on purpose: a different message for each
    /// would let one customer enumerate another's.
    pub fn url_for(&self, user_id: &str, name: &str) -> Result<String> {
        self.by_user
            .get(user_id)
            .and_then(|m| m.get(name))
            .cloned()
            .ok_or_else(|| LiveSourceError::invalid("등록되지 않은 송출 대상입니다."))
    }

    pub fn user_count(&self) -> usize {
        self.by_user.len()
    }
}

fn name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.chars().count() <= 40
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "rtmps://a.rtmps.youtube.com/live2/alice-key-aaaa-bbbb";
    const KEY_B: &str = "rtmp://127.0.0.1:1935/live/bob-key-cccc-dddd";

    fn two_users() -> Destinations {
        Destinations::parse(&format!(
            r#"{{"user-alice":{{"alice-main":"{KEY_A}"}},"user-bob":{{"bob-test":"{KEY_B}"}}}}"#
        ))
        .unwrap()
    }

    #[test]
    fn a_user_gets_their_own_destination() {
        let d = two_users();
        assert_eq!(d.url_for("user-alice", "alice-main").unwrap(), KEY_A);
        assert_eq!(d.url_for("user-bob", "bob-test").unwrap(), KEY_B);
    }

    #[test]
    fn a_user_cannot_reach_another_users_destination_by_name() {
        // The defect this module closes: a flat map let anybody use anybody's.
        let d = two_users();
        let e = d.url_for("user-bob", "alice-main").unwrap_err();
        assert_eq!(e.message, "등록되지 않은 송출 대상입니다.");
        assert!(d.url_for("user-alice", "bob-test").is_err());
        // And a user with no destinations at all gets the same answer, so the
        // message is not an oracle.
        assert_eq!(
            d.url_for("user-nobody", "alice-main").unwrap_err().message,
            "등록되지 않은 송출 대상입니다."
        );
    }

    #[test]
    fn a_user_cannot_even_see_another_users_names() {
        let d = two_users();
        assert_eq!(d.names_for("user-alice"), vec!["alice-main".to_string()]);
        assert_eq!(d.names_for("user-bob"), vec!["bob-test".to_string()]);
        assert!(d.names_for("user-nobody").is_empty());
        // A name is itself a leak: it says another customer exists and is
        // enough to try sending to.
        assert!(!d.names_for("user-bob").contains(&"alice-main".to_string()));
    }

    #[test]
    fn nothing_hands_out_a_url() {
        let d = two_users();
        // No `Debug`, no listing, no error message carries one.
        let debug = format!("{d:?}");
        assert_eq!(debug, "Destinations(2 users)");
        for s in [
            debug,
            format!("{:?}", d.names_for("user-alice")),
            d.url_for("user-bob", "alice-main").unwrap_err().message,
            Destinations::parse("not json").unwrap_err().message,
        ] {
            for needle in ["rtmp", "alice-key", "bob-key", "live2"] {
                assert!(!s.contains(needle), "{needle} leaked in {s}");
            }
        }
        // The one message that does name the scheme is the startup refusal, and
        // it names it without quoting any configured value.
        let shape = Destinations::parse(r#"{"u":{"a":"file:///etc/passwd"}}"#).unwrap_err().message;
        assert!(shape.contains("rtmp://"), "the operator needs to be told the shape: {shape}");
        assert!(!shape.contains("passwd"), "{shape}");
    }

    #[test]
    fn a_malformed_configuration_is_refused_without_being_echoed() {
        // Each entry is paired with the part of itself that must not come back
        // out. Not the literal "rtmp": the refusal legitimately *says* what an
        // ingest address has to start with, and asserting on that would be
        // asserting the message cannot explain itself.
        for (raw, secret) in [
            ("not json", "not json"),
            ("[]", "[]"),
            ("{}", "{}"),
            (r#"{"":{"a":"rtmp://x/live2/KEY-aaaa"}}"#, "KEY-aaaa"),
            (r#"{"u":{}}"#, r#"{"u":{}}"#),
            // A name that could become a path or an argument.
            (r#"{"u":{"../x":"rtmp://x/live2/KEY-bbbb"}}"#, "KEY-bbbb"),
            (r#"{"u":{"a b":"rtmp://x/live2/KEY-cccc"}}"#, "KEY-cccc"),
            (r#"{"u":{"":"rtmp://x/live2/KEY-dddd"}}"#, "KEY-dddd"),
            // Not an ingest at all.
            (r#"{"u":{"a":"file:///etc/passwd"}}"#, "passwd"),
            (r#"{"u":{"a":"http://evil.example/x"}}"#, "evil.example"),
            (r#"{"u":{"a":"rtmp://x/live2/KEY eeee"}}"#, "KEY eeee"),
        ] {
            let e = Destinations::parse(raw).unwrap_err();
            assert_eq!(e.kind, crate::ErrorKind::Invalid, "{raw}");
            // Whatever was in the configuration must not come back out.
            assert!(!e.message.contains(secret), "{raw} → {}", e.message);
            assert!(!e.message.contains("live2"), "{raw} → {}", e.message);
        }
    }

    #[test]
    fn a_long_or_odd_name_is_refused_at_startup() {
        let long = "x".repeat(41);
        assert!(Destinations::parse(&format!(r#"{{"u":{{"{long}":"rtmp://x/y"}}}}"#)).is_err());
        let ok = "x".repeat(40);
        assert!(Destinations::parse(&format!(r#"{{"u":{{"{ok}":"rtmp://x/y"}}}}"#)).is_ok());
    }
}
