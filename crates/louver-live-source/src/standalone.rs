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
//!  2. Neither it nor `--allow-origin` may name the
//!     [service's domain](PRODUCTION_DOMAIN) or any subdomain of it.
//!  3. `--allow-origin` must name the same origin as `--origin` — in the
//!     standalone arrangement the page and `/api/me` are the same host, so
//!     anything else is a mistake, and the mistake that matters is setting the
//!     browser origin to the test host while leaving the authentication origin
//!     at its production default.
//!
//! ## Why the whole domain, and why comparison needs normalising
//!
//! The apex alone is not enough. A subdomain under the service's domain is
//! resolved by the service's DNS and, with a wildcard record or a stray `A`,
//! can be made to answer — so an origin there is not reliably a test host, and
//! a test box should not wear the service's name in any case.
//!
//! Matching is on a **label boundary**, never a substring: `evil-247streams.kr`
//! and `247streams.kr.evil.test` are other people's domains and are allowed.
//! Before any comparison the host is normalised, because each of these is the
//! same origin to a browser or a resolver and a different string to `==`:
//!
//! | written | normalised |
//! |---|---|
//! | `https://247STREAMS.KR` | case-folded |
//! | `https://247streams.kr.` | the root's trailing dot removed |
//! | `https://247streams.kr:443` | the default https port removed |
//! | `https://247streams.kr/` | the trailing slash removed |
//!
//! Normalising is not only for the refusal. The canonical form is what gets
//! handed to `AllowedOrigins`, which compares byte for byte against the
//! browser's `Origin` header — and a browser sends a lower-case host, no
//! trailing dot and no `:443`. So an operator who writes any of those spellings
//! for a *legitimate* test host now gets a configuration that matches real
//! requests instead of one that silently refuses every session.
//!
//! What is deliberately **not** blocked: any other domain, and `localhost` or a
//! loopback address, so that a developer can run the whole arrangement on one
//! machine. `https://` is still required, because that is what `ProductionMe`
//! accepts and this module does not widen it.
//!
//! None of this runs unless [`FLAG`] is passed. Without it the binary parses
//! these two options exactly as before — same default, same fallback — because
//! that path serves customers and this one serves a test.

use crate::error::{LiveSourceError, Result};

/// Opt in to the checks in this module. Nothing here applies without it.
pub const FLAG: &str = "--standalone-test";

/// The service's own domain. This and every subdomain of it are refused as a
/// standalone test origin.
pub const PRODUCTION_DOMAIN: &str = "247streams.kr";

/// Where authentication goes and where a browser may come from, both checked
/// and both canonical, so everything downstream agrees: `ProductionMe` tolerates
/// a trailing slash, `AllowedOrigins` refuses one, and a browser sends neither
/// that nor a trailing dot, an upper-case host or an explicit `:443`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origins {
    /// `{auth}/api/me` is asked who a cookie belongs to.
    pub auth: String,
    /// The origin a session may be started from.
    pub allow: String,
}

/// Was [`FLAG`] passed?
pub fn is_enabled<S: AsRef<str>>(args: &[S]) -> bool {
    args.iter().any(|a| a.as_ref() == FLAG)
}

/// Is this host the service's domain, or under it?
///
/// Takes a host, not an origin, and matches on a label boundary so that a
/// domain which merely ends with the same characters is somebody else's.
pub fn is_production_domain(host: &str) -> bool {
    let h = normalize_host(host);
    h == PRODUCTION_DOMAIN || h.ends_with(&format!(".{PRODUCTION_DOMAIN}"))
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
             생략하면 운영 인증 서버(https://{PRODUCTION_DOMAIN})가 기본값으로 쓰입니다."
        ))
    })?;

    let auth = canonical(raw)?;
    refuse_production(&auth, "--origin")?;

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
                // The domain policy is checked per entry, before the equality
                // check, so a production origin here is named as that rather
                // than as a mismatch.
                refuse_production(&one, "--allow-origin")?;
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

    Ok(Origins { auth, allow })
}

/// Refuse the service's domain, naming which option carried it.
fn refuse_production(canonical_origin: &str, option: &str) -> Result<()> {
    let host = host_of(canonical_origin).ok_or_else(|| {
        LiveSourceError::invalid(format!("{FLAG}: {option} 에서 호스트를 읽을 수 없습니다."))
    })?;
    if is_production_domain(host) {
        return Err(LiveSourceError::invalid(format!(
            "{FLAG} 에서는 운영 도메인({PRODUCTION_DOMAIN}) 및 그 하위 도메인을 {option} 으로 쓸 수 없습니다. \
             (받은 호스트: {host}) 테스트 전용 도메인을 지정해 주세요."
        )));
    }
    Ok(())
}

/// The canonical spelling of an origin: the shape `ProductionMe` demands, plus
/// every normalisation a browser or a resolver would apply anyway.
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

    let (host, port) = split_port(rest);
    let host = normalize_host(host);
    if host.is_empty() {
        return Err(shape());
    }
    // `:443` is the default for https and a browser never sends it, so keeping
    // it would make a correct configuration fail to match real requests.
    let port = port.filter(|p| *p != "443");
    Ok(match port {
        Some(p) => format!("https://{host}:{p}"),
        None => format!("https://{host}"),
    })
}

/// Split `host[:port]`, treating only an all-digit tail as a port so an IPv6
/// literal keeps its colons.
fn split_port(authority: &str) -> (&str, Option<&str>) {
    match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host, Some(port))
        }
        _ => (authority, None),
    }
}

/// Case-folded, and without the root's trailing dot. `247streams.kr.` and
/// `247streams.kr` are the same name to a resolver.
fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// The host of an already-canonical origin, without its port.
fn host_of(origin: &str) -> Option<&str> {
    let rest = origin.strip_prefix("https://")?;
    Some(split_port(rest).0)
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
    }

    #[test]
    fn a_missing_origin_is_refused_and_the_message_names_the_default_it_prevents() {
        for missing in [None, Some(""), Some("   ")] {
            let e = check(missing, Some(TEST_HOST)).unwrap_err();
            assert!(e.message.contains("--origin"), "{}", e.message);
            assert!(e.message.contains(FLAG), "{}", e.message);
            // The operator is told what the default would have been.
            assert!(e.message.contains(PRODUCTION_DOMAIN), "{}", e.message);
        }
    }

    /// Every spelling of the service's domain that normalisation could let
    /// through. Each is the same origin to a browser or a resolver.
    const PRODUCTION_SPELLINGS: &[&str] = &[
        "https://247streams.kr",
        "https://247streams.kr/",
        "https://247streams.kr//",
        "https://247STREAMS.KR",
        "https://247Streams.Kr",
        "https://247streams.kr.",
        "https://247streams.kr./",
        "https://247STREAMS.KR.",
        "https://247streams.kr:443",
        "https://247streams.kr.:443",
        "https://247STREAMS.KR:443/",
        "https://247streams.kr:8443",
        "  https://247streams.kr  ",
        // subdomains, including the ones a wildcard record would answer
        "https://www.247streams.kr",
        "https://WWW.247Streams.KR",
        "https://api.247streams.kr",
        "https://beta-test.247streams.kr",
        "https://a.b.c.247streams.kr",
        "https://beta.247streams.kr.:443/",
    ];

    #[test]
    fn the_service_domain_is_refused_in_every_spelling_as_the_auth_origin() {
        for bad in PRODUCTION_SPELLINGS {
            let e = check(Some(bad), None).unwrap_err();
            assert!(e.message.contains("운영 도메인"), "{bad}: {}", e.message);
            assert!(e.message.contains("--origin"), "{bad}: {}", e.message);
        }
    }

    #[test]
    fn the_service_domain_is_refused_in_every_spelling_as_the_browser_origin() {
        // Requirement in its own right: the same policy on `--allow-origin`,
        // and the message names that option rather than reporting a mismatch.
        for bad in PRODUCTION_SPELLINGS {
            let e = check(Some(TEST_HOST), Some(bad)).unwrap_err();
            assert!(e.message.contains("운영 도메인"), "{bad}: {}", e.message);
            assert!(e.message.contains("--allow-origin"), "{bad}: {}", e.message);
        }
        // Also when it is one entry of a list that is otherwise fine.
        let e = check(Some(TEST_HOST), Some("https://beta-test.example.com,https://www.247streams.kr"))
            .unwrap_err();
        assert!(e.message.contains("운영 도메인"), "{}", e.message);
    }

    #[test]
    fn a_production_origin_is_refused_even_when_allow_origin_looks_like_a_test() {
        // The exact accident this guard exists for: every visible setting says
        // test, and authentication alone still points at the live service.
        let e = check(Some("https://247streams.kr"), Some(TEST_HOST)).unwrap_err();
        assert!(e.message.contains("운영 도메인"), "{}", e.message);
    }

    #[test]
    fn a_domain_that_merely_looks_similar_belongs_to_somebody_else() {
        // Matching is on a label boundary, so none of these is the service's
        // domain and blocking them would be wrong.
        for ok in [
            "https://evil-247streams.kr",
            "https://my247streams.kr",
            "https://247streams.kr.evil.test",
            "https://247streams.com",
            "https://not247streams.kr",
            "https://x247streams.kr",
        ] {
            let got = check(Some(ok), None);
            assert!(got.is_ok(), "{ok} should be allowed: {:?}", got.err().map(|e| e.message));
        }
    }

    #[test]
    fn a_separate_test_domain_and_a_localhost_setup_are_not_blocked() {
        // Requirement: the legitimate configurations must keep working.
        for ok in [
            "https://beta-test.example.com",
            "https://beta-test.example.com:8443",
            "https://live-source-test.co.kr",
            "https://localhost",
            "https://localhost:8443",
            "https://127.0.0.1",
            "https://127.0.0.1:9443",
            "https://[::1]",
            "https://[::1]:8443",
        ] {
            let got = check(Some(ok), Some(ok));
            assert!(got.is_ok(), "{ok} should be allowed: {:?}", got.err().map(|e| e.message));
        }
    }

    #[test]
    fn normalising_makes_the_spellings_of_one_test_host_agree() {
        // The same host written four ways is one origin, and the canonical form
        // is the one a browser actually sends.
        for spelling in [
            "https://Beta-Test.Example.com",
            "https://beta-test.example.com.",
            "https://beta-test.example.com:443",
            "https://beta-test.example.com/",
        ] {
            let o = check(Some(spelling), None).unwrap();
            assert_eq!(o.auth, TEST_HOST, "{spelling}");
        }
        // And so `--origin` and `--allow-origin` written differently still
        // agree, instead of being refused as a mismatch.
        let o =
            check(Some("https://Beta-Test.Example.com:443"), Some("https://beta-test.example.com.")).unwrap();
        assert_eq!(o.auth, TEST_HOST);
        assert_eq!(o.allow, TEST_HOST);
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

        // A non-default port is part of the origin, so it has to agree too.
        let e = check(Some(TEST_HOST), Some("https://beta-test.example.com:8443")).unwrap_err();
        assert!(e.message.contains("--allow-origin"), "{}", e.message);
    }

    #[test]
    fn the_canonical_form_is_one_both_downstream_parsers_accept() {
        // Proof rather than assertion: a trailing slash is fine for
        // `ProductionMe` and fatal for `AllowedOrigins`, so the canonical form
        // has to satisfy both.
        for spelling in
            ["https://beta-test.example.com/", "https://beta-test.example.com:443", "https://B.example.com."]
        {
            let o = check(Some(spelling), None).unwrap();
            assert!(crate::auth::ProductionMe::new(&o.auth).is_ok(), "{spelling}");
            assert!(crate::origin::AllowedOrigins::parse(&o.allow).is_ok(), "{spelling}");
        }
    }

    #[test]
    fn a_shape_that_could_not_be_an_origin_is_refused() {
        for bad in [
            "http://beta-test.example.com",
            "beta-test.example.com",
            "https://",
            "https://.",
            "https://user@beta-test.example.com",
            "https://beta-test.example.com/api/me",
            "https://beta-test.example.com/?x=1",
            "file:///etc/passwd",
        ] {
            assert!(check(Some(bad), None).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_domain_test_takes_a_host_and_matches_on_a_label() {
        assert!(is_production_domain("247streams.kr"));
        assert!(is_production_domain("247STREAMS.KR."));
        assert!(is_production_domain("www.247streams.kr"));
        assert!(!is_production_domain("evil-247streams.kr"));
        assert!(!is_production_domain("247streams.kr.evil.test"));
        assert!(!is_production_domain("beta-test.example.com"));
        assert!(!is_production_domain("localhost"));
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
