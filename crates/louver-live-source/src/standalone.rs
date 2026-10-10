//! The guard between a standalone test box and production's identity server.
//!
//! The worker's only link to production is one call: `GET {origin}/api/me`,
//! asking who a session cookie belongs to. `--origin` **defaults to the
//! production host**, because that is the right default for the configuration
//! this crate was written for — production's Caddy in front, the beta page
//! served from production's origin, a real cookie in the request.
//!
//! A standalone test box is the opposite arrangement: its own Caddy, its own
//! `/api/me`, no production anything. There the default is exactly wrong, and
//! wrong in a quiet way — omit `--origin` from a unit file and the worker will
//! send whatever cookie it is given to the live service, while every other
//! setting still looks like a test configuration.
//!
//! So the test configuration has to say so. [`FLAG`] turns on the checks in
//! this module, and they are deliberately the kind that refuse to start:
//!
//!  1. `--origin` becomes **mandatory**, because the hazard is the default.
//!  2. Its host may not be a [production host](PRODUCTION_HOSTS).
//!  3. `--allow-origin`, which says where a browser may start a session, must
//!     name the same origin — in the standalone arrangement the page and
//!     `/api/me` are the same host, so anything else is a mistake, and the
//!     mistake that matters is setting the browser origin to the test host
//!     while leaving the authentication origin at its production default.
//!
//! None of this runs unless [`FLAG`] is passed. Without it the binary parses
//! these two options exactly as before — same default, same fallback — because
//! that path serves customers and this one serves a test.

use crate::error::{LiveSourceError, Result};

/// Opt in to the checks in this module. Nothing here applies without it.
pub const FLAG: &str = "--standalone-test";

/// The hosts that answer for the live service. An origin naming one of these is
/// production's identity server whatever the rest of the configuration says.
pub const PRODUCTION_HOSTS: &[&str] = &["247streams.kr", "www.247streams.kr"];

/// Hosts under the service's own domain. Not refused — a subdomain is not
/// production's authentication server — but worth saying out loud, because the
/// runbook asks for a separate domain so that a test box never wears the
/// service's name.
pub const PRODUCTION_DOMAIN_SUFFIX: &str = ".247streams.kr";

/// Where authentication goes and where a browser may come from, both checked
/// and both canonical, so the two parsers downstream accept them: `ProductionMe`
/// tolerates a trailing slash and `AllowedOrigins` refuses one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origins {
    /// `{auth}/api/me` is asked who a cookie belongs to.
    pub auth: String,
    /// The origin a session may be started from.
    pub allow: String,
    /// Printed at startup when something is allowed but questionable.
    pub warning: Option<String>,
}

/// Was [`FLAG`] passed?
pub fn is_enabled<S: AsRef<str>>(args: &[S]) -> bool {
    args.iter().any(|a| a.as_ref() == FLAG)
}

/// Check the two origin options for a standalone test box.
///
/// Takes `Option` for each so a missing option is a case this function decides
/// rather than one a caller might forget — the missing `--origin` is the whole
/// reason this exists.
pub fn check(origin: Option<&str>, allow_origin: Option<&str>) -> Result<Origins> {
    let raw = origin.map(str::trim).filter(|o| !o.is_empty()).ok_or_else(|| {
        LiveSourceError::invalid(format!(
            "{FLAG} 를 쓸 때는 --origin 을 반드시 지정해야 합니다. \
             생략하면 운영 인증 서버(https://{})가 기본값으로 쓰입니다.",
            PRODUCTION_HOSTS[0]
        ))
    })?;

    let auth = canonical(raw)?;
    let host = host_of(&auth)
        .ok_or_else(|| LiveSourceError::invalid(format!("{FLAG}: --origin 에서 호스트를 읽을 수 없습니다.")))?
        .to_ascii_lowercase();

    if PRODUCTION_HOSTS.contains(&host.as_str()) {
        return Err(LiveSourceError::invalid(format!(
            "{FLAG} 에서는 운영 호스트({host})를 --origin 으로 쓸 수 없습니다. \
             테스트 VPS 자신의 호스트를 지정해 주세요."
        )));
    }

    let warning = host.ends_with(PRODUCTION_DOMAIN_SUFFIX).then(|| {
        format!(
            "경고: --origin 호스트({host})가 서비스 도메인 아래에 있습니다. 테스트 전용 도메인을 권장합니다."
        )
    });

    let allow = match allow_origin.map(str::trim).filter(|a| !a.is_empty()) {
        // Omitted is safe: it falls back to an origin this function just
        // checked, which is the whole point of checking `--origin` first.
        None => auth.clone(),
        Some(list) => {
            let mut matched = 0usize;
            for part in list.split(',') {
                let entry = part.trim();
                if entry.is_empty() {
                    continue;
                }
                let one = canonical(entry)?;
                if one != auth {
                    return Err(LiveSourceError::invalid(format!(
                        "{FLAG} 에서는 --allow-origin 이 --origin 과 같아야 합니다. \
                         (--origin {auth}, --allow-origin {one})"
                    )));
                }
                matched += 1;
            }
            if matched == 0 {
                return Err(LiveSourceError::invalid(format!(
                    "{FLAG}: --allow-origin 에 유효한 항목이 없습니다."
                )));
            }
            auth.clone()
        }
    };

    Ok(Origins { auth, allow, warning })
}

/// One trailing slash removed, and the same shape `ProductionMe` demands —
/// checked here too so the refusal names the mode the operator asked for.
fn canonical(origin: &str) -> Result<String> {
    let shape = || {
        LiveSourceError::invalid(format!(
            "{FLAG} 에서는 origin 이 https:// 로 시작하는 호스트여야 합니다. ({origin})"
        ))
    };
    let trimmed = origin.trim().trim_end_matches('/');
    let rest = trimmed.strip_prefix("https://").ok_or_else(shape)?;
    if rest.is_empty() || rest.contains('/') || rest.contains('@') || rest.contains('?') {
        return Err(shape());
    }
    Ok(trimmed.to_string())
}

/// The host out of an already-canonical origin, without its port.
fn host_of(origin: &str) -> Option<&str> {
    let rest = origin.strip_prefix("https://")?;
    // Only a numeric tail is a port, so an IPv6 literal keeps its colons.
    Some(match rest.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => rest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_HOST: &str = "https://beta-test.example.com";

    #[test]
    fn the_flag_is_what_turns_any_of_this_on() {
        assert!(!is_enabled::<String>(&[]));
        assert!(!is_enabled(&["--origin".to_string(), TEST_HOST.to_string()]));
        assert!(is_enabled(&["--standalone-test".to_string()]));
        assert!(is_enabled(&["--listen".to_string(), "x".to_string(), FLAG.to_string()]));
    }

    #[test]
    fn a_test_origin_is_accepted_and_allow_origin_follows_it() {
        let o = check(Some(TEST_HOST), None).unwrap();
        assert_eq!(o.auth, TEST_HOST);
        // Omitted `--allow-origin` falls back to the checked origin, not to a
        // production default.
        assert_eq!(o.allow, TEST_HOST);
        assert_eq!(o.warning, None);
    }

    #[test]
    fn a_missing_origin_is_refused_and_the_message_names_the_default_it_prevents() {
        for missing in [None, Some(""), Some("   ")] {
            let e = check(missing, Some(TEST_HOST)).unwrap_err();
            assert!(e.message.contains("--origin"), "{}", e.message);
            assert!(e.message.contains(FLAG), "{}", e.message);
            // The operator is told what the default would have been.
            assert!(e.message.contains("247streams.kr"), "{}", e.message);
        }
    }

    #[test]
    fn the_production_host_is_refused_however_it_is_written() {
        for bad in [
            "https://247streams.kr",
            "https://247streams.kr/",
            "https://247STREAMS.kr",
            "https://247streams.kr:443",
            "https://www.247streams.kr",
        ] {
            let e = check(Some(bad), None).unwrap_err();
            assert!(e.message.contains("운영 호스트"), "{bad}: {}", e.message);
        }
    }

    #[test]
    fn a_production_origin_is_refused_even_when_allow_origin_looks_like_a_test() {
        // The exact accident this guard exists for: every visible setting says
        // test, and authentication alone still points at the live service.
        let e = check(Some("https://247streams.kr"), Some(TEST_HOST)).unwrap_err();
        assert!(e.message.contains("운영 호스트"), "{}", e.message);
    }

    #[test]
    fn allow_origin_has_to_name_the_same_origin() {
        let e = check(Some(TEST_HOST), Some("https://somewhere-else.example.com")).unwrap_err();
        assert!(e.message.contains("--allow-origin"), "{}", e.message);
        assert!(e.message.contains(TEST_HOST), "{}", e.message);

        // A list is fine only if every entry is that origin.
        let e = check(Some(TEST_HOST), Some("https://beta-test.example.com,https://evil.example.com"))
            .unwrap_err();
        assert!(e.message.contains("--allow-origin"), "{}", e.message);
        let ok =
            check(Some(TEST_HOST), Some("https://beta-test.example.com, https://beta-test.example.com/"))
                .unwrap();
        assert_eq!(ok.allow, TEST_HOST);

        // A port is part of the origin, so it has to agree too.
        let e = check(Some(TEST_HOST), Some("https://beta-test.example.com:8443")).unwrap_err();
        assert!(e.message.contains("--allow-origin"), "{}", e.message);
    }

    #[test]
    fn a_trailing_slash_is_canonicalised_so_both_parsers_accept_it() {
        let o = check(Some("https://beta-test.example.com/"), None).unwrap();
        assert_eq!(o.auth, TEST_HOST);
        // Proof rather than assertion: the two downstream parsers accept it.
        assert!(crate::auth::ProductionMe::new(&o.auth).is_ok());
        assert!(crate::origin::AllowedOrigins::parse(&o.allow).is_ok());
    }

    #[test]
    fn a_shape_that_could_not_be_an_origin_is_refused() {
        for bad in [
            "http://beta-test.example.com",
            "beta-test.example.com",
            "https://",
            "https://user@beta-test.example.com",
            "https://beta-test.example.com/api/me",
            "https://beta-test.example.com/?x=1",
            "file:///etc/passwd",
        ] {
            assert!(check(Some(bad), None).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_host_under_the_service_domain_starts_but_says_so() {
        let o = check(Some("https://beta-test.247streams.kr"), None).unwrap();
        assert_eq!(o.auth, "https://beta-test.247streams.kr");
        let w = o.warning.expect("a subdomain of the service domain should be remarked on");
        assert!(w.contains("경고"), "{w}");
        // A warning, not a refusal: it is not production's identity server.
        assert!(!PRODUCTION_HOSTS.contains(&"beta-test.247streams.kr"));
    }

    #[test]
    fn a_port_is_kept_but_is_not_read_as_part_of_the_host() {
        let o = check(Some("https://beta-test.example.com:8443"), None).unwrap();
        assert_eq!(o.auth, "https://beta-test.example.com:8443");
        assert_eq!(host_of(&o.auth), Some("beta-test.example.com"));
        assert_eq!(host_of("https://[::1]"), Some("[::1]"));
        assert_eq!(host_of("https://[::1]:443"), Some("[::1]"));
    }
}
