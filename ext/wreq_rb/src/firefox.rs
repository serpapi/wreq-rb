//! Firefox profiles newer than the ones shipped with the pinned `wreq-util`.
//!
//! `wreq-util` 0.2.0 stops at Firefox 151, so the releases after it are defined
//! here: each one starts from the bundled Firefox 151 profile and overrides the
//! parts that differ on the wire. A name that `wreq-util` knows always resolves
//! to its profile, so once it ships one of these releases the entry here is
//! dead and should be removed (a unit test fails when that happens).
//!
//! The bundled Firefox 151 profile is itself slightly off, so three corrections
//! to the TLS and HTTP/2 fingerprint apply to every profile in this module
//! (upstream `wreq-util` has since made the same ones on its main branch):
//!
//! - `TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA` is no longer offered;
//! - `Accept-Language` is `en-US,en;q=0.9` rather than `q=0.5` (Bugzilla 2000765);
//! - the HEADERS frame carries priority weight 42 rather than 22. Firefox
//!   derives the weight from the request's urgency: 42 goes with the
//!   navigation request these headers describe (`priority: u=0, i`), 22 with
//!   lower-urgency requests such as the favicon.
//!
//! The bundled profile also leaves the HTTP/1.1 request to the HTTP stack,
//! which writes lower-case header names with `host` last. The profiles here
//! carry the names as Firefox spells and orders them ([`HEADER_ORDER`]) and
//! add `Connection: keep-alive`, which only reaches the wire over HTTP/1.1.
//!
//! On top of that the cipher suites and groups changed twice between 152 and
//! 158. The versions come from the Firefox sources at the
//! `FIREFOX_<version>_0_RELEASE` tags:
//!
//! - 154 turned off `security.ssl3.ecdhe_ecdsa_aes_256_sha` on release builds
//!   (`StaticPrefList.yaml`), dropping `TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA`;
//! - 156 stopped advertising `ffdhe2048`/`ffdhe3072` in `supported_groups`
//!   unless a DHE cipher suite is enabled, and none is (`nsNSSIOLayer.cpp`).
//!
//! Every profile was compared against the official macOS release build of the
//! same version (152.0 to 158.0) making a top-level navigation. JA3, JA4, the
//! Akamai HTTP/2 fingerprint, the cipher suites, the extension order and
//! bodies, groups, key share groups, signature algorithms, the HTTP/2
//! SETTINGS, WINDOW_UPDATE and HEADERS priority, the header names, values and
//! order, and the HTTP/1.1 request all match. 157 hard-coded the P-256 key
//! share and added ML-DSA signature schemes behind `security.tls.enable_mldsa`,
//! which is off, so its ClientHello is the same as 156's.
//!
//! Known gaps. On a desktop platform, and with HTTP/2 left on, none of them is
//! visible in JA3, JA4 or the Akamai fingerprint:
//!
//! - The GREASE ECH extension. Firefox sends a fixed payload of 239 bytes up
//!   to 154 and 240 from 155 (NSS 3.127), while BoringSSL picks 144, 176, 208
//!   or 240 at random, so the ClientHello length differs. Firefox also fills
//!   `enc` with random bytes where BoringSSL sends a real X25519 public key.
//! - Firefox reuses its X25519 key inside the X25519MLKEM768 key share;
//!   BoringSSL generates two independent keys.
//! - HTTP/2: Firefox sends a WINDOW_UPDATE for the stream right after HEADERS
//!   and never encodes `:path` as an indexed header field; the weight is the
//!   same for every request (see above).
//! - HTTP/1.1: `TE: trailers` is sent as well, also to `http://` URLs, where
//!   Firefox sends it over HTTP/2 only. A client created with `http1_only`
//!   leaves it out, but such a client offers only `http/1.1` in ALPN, which
//!   changes JA4. Over TLS it is one or the other.
//! - The header order is that of a navigation. Firefox orders a `fetch()`
//!   request differently: `Referer` moves up to follow `Accept-Encoding`. The
//!   `header_order` client option takes precedence over the profile's order.
//!   A profile chosen for a single request brings its own order and, like
//!   every per-request profile, ignores the client's `header_order`.
//! - The navigation headers are always sent. A link click or a form
//!   submission made by a script has no `Sec-Fetch-User`, for instance, and
//!   there is no way to leave a default header out.
//! - These are desktop fingerprints. Android and iOS platforms only swap the
//!   User-Agent, although Firefox for Android uses different HTTP/2 settings.

use std::borrow::Cow;

use wreq::header::{
    HeaderName, HeaderValue, OrigHeaderMap, ACCEPT_LANGUAGE, CONNECTION, CONTENT_TYPE, TE, USER_AGENT,
};
use wreq::http2::{StreamDependency, StreamId};
use wreq::{Group, IntoEmulation};
use wreq_util::{Emulation as BrowserEmulation, Platform, Profile as BrowserProfile};

/// The bundled profile every profile in this module is derived from.
const BASE_PROFILE: BrowserProfile = BrowserProfile::Firefox151;

/// The version as it appears in [`BASE_PROFILE`]'s User-Agent strings.
const BASE_VERSION: &str = "151.0";

const ACCEPT_LANGUAGE_VALUE: &str = "en-US,en;q=0.9";

/// Weight of the HEADERS frame priority, one less than the value on the wire.
const HEADERS_PRIORITY_WEIGHT: u8 = 41;

/// Request header names in the order Firefox sends them, spelled the way it
/// writes them in an HTTP/1.1 request.
///
/// The order is that of a top-level navigation, recorded from Firefox 152 and
/// 158 over HTTP/1.1: a plain GET, a GET carrying a referrer, cookies and
/// cache validators, a reload and a form POST. HTTP/2 uses the same order
/// without `Host` and `Connection` and ends with `TE` (recorded for the GETs
/// and the form POST). Two groups of names do not come from those recordings:
///
/// - `Authorization` and `Range` were only seen on `fetch()` requests, which
///   put `Referer` and then the headers the script sets, in the script's
///   order, right after `Accept-Encoding`. Their place here is the same one
///   for a request without a referrer. The `Authorization` header Firefox
///   adds itself after an HTTP authentication challenge comes later, right
///   before `Connection` (`nsHttpChannel.cpp`).
/// - `DNT` and `Sec-GPC` follow the Firefox sources, which add them between
///   `Origin` and `Connection` (`nsHttpChannel.cpp`).
///
/// A header that is not listed is written right before
/// [`UNLISTED_HEADERS_BEFORE`], spelled as the caller spelled it. If the
/// `header_order` option claims that name, it is written before the next
/// listed name the option leaves alone.
const HEADER_ORDER: &[&str] = &[
    "Host",
    "User-Agent",
    "Accept",
    "Accept-Language",
    "Accept-Encoding",
    "Authorization",
    "Range",
    "Content-Type",
    "Content-Length",
    "Origin",
    "DNT",
    "Sec-GPC",
    "Connection",
    "Referer",
    "Cookie",
    "Upgrade-Insecure-Requests",
    "Sec-Fetch-Dest",
    "Sec-Fetch-Mode",
    "Sec-Fetch-Site",
    "Sec-Fetch-User",
    "If-Modified-Since",
    "If-None-Match",
    "Priority",
    "Pragma",
    "Cache-Control",
    "TE",
];

/// Headers that [`HEADER_ORDER`] does not name go right before this one, in
/// the order the caller gave them: the place where Firefox writes the headers
/// a script passes to `fetch()`.
pub const UNLISTED_HEADERS_BEFORE: HeaderName = CONTENT_TYPE;

/// Cipher suites offered by Firefox 152 and 153.
const CIPHERS_152: &str = concat!(
    "TLS_AES_128_GCM_SHA256:",
    "TLS_CHACHA20_POLY1305_SHA256:",
    "TLS_AES_256_GCM_SHA384:",
    "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:",
    "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:",
    "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:",
    "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:",
    "TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA:",
    "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:",
    "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:",
    "TLS_RSA_WITH_AES_128_GCM_SHA256:",
    "TLS_RSA_WITH_AES_256_GCM_SHA384:",
    "TLS_RSA_WITH_AES_128_CBC_SHA:",
    "TLS_RSA_WITH_AES_256_CBC_SHA",
);

/// Cipher suites offered since Firefox 154, which dropped
/// `TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA`.
const CIPHERS_154: &str = concat!(
    "TLS_AES_128_GCM_SHA256:",
    "TLS_CHACHA20_POLY1305_SHA256:",
    "TLS_AES_256_GCM_SHA384:",
    "TLS_ECDHE_ECDSA_WITH_AES_128_GCM_SHA256:",
    "TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256:",
    "TLS_ECDHE_ECDSA_WITH_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_RSA_WITH_CHACHA20_POLY1305_SHA256:",
    "TLS_ECDHE_ECDSA_WITH_AES_256_GCM_SHA384:",
    "TLS_ECDHE_RSA_WITH_AES_256_GCM_SHA384:",
    "TLS_ECDHE_RSA_WITH_AES_128_CBC_SHA:",
    "TLS_ECDHE_RSA_WITH_AES_256_CBC_SHA:",
    "TLS_RSA_WITH_AES_128_GCM_SHA256:",
    "TLS_RSA_WITH_AES_256_GCM_SHA384:",
    "TLS_RSA_WITH_AES_128_CBC_SHA:",
    "TLS_RSA_WITH_AES_256_CBC_SHA",
);

/// Supported groups advertised up to Firefox 155.
const GROUPS_152: &str = "X25519MLKEM768:X25519:P-256:P-384:P-521:ffdhe2048:ffdhe3072";

/// Supported groups advertised since Firefox 156, which dropped the FFDHE groups.
const GROUPS_156: &str = "X25519MLKEM768:X25519:P-256:P-384:P-521";

/// A Firefox release that `wreq-util` has no profile for.
#[derive(Debug)]
pub struct FirefoxProfile {
    /// The name accepted by the `emulation` option, e.g. `firefox_158`.
    name: &'static str,
    /// The version as it appears in the User-Agent, e.g. `158.0`.
    version: &'static str,
    cipher_list: &'static str,
    curves_list: &'static str,
}

const PROFILES: &[FirefoxProfile] = &[
    FirefoxProfile {
        name: "firefox_152",
        version: "152.0",
        cipher_list: CIPHERS_152,
        curves_list: GROUPS_152,
    },
    FirefoxProfile {
        name: "firefox_153",
        version: "153.0",
        cipher_list: CIPHERS_152,
        curves_list: GROUPS_152,
    },
    FirefoxProfile {
        name: "firefox_154",
        version: "154.0",
        cipher_list: CIPHERS_154,
        curves_list: GROUPS_152,
    },
    FirefoxProfile {
        name: "firefox_155",
        version: "155.0",
        cipher_list: CIPHERS_154,
        curves_list: GROUPS_152,
    },
    FirefoxProfile {
        name: "firefox_156",
        version: "156.0",
        cipher_list: CIPHERS_154,
        curves_list: GROUPS_156,
    },
    FirefoxProfile {
        name: "firefox_157",
        version: "157.0",
        cipher_list: CIPHERS_154,
        curves_list: GROUPS_156,
    },
    FirefoxProfile {
        name: "firefox_158",
        version: "158.0",
        cipher_list: CIPHERS_154,
        curves_list: GROUPS_156,
    },
];

impl FirefoxProfile {
    /// Look up a profile by its `emulation` name.
    pub fn find(name: &str) -> Option<&'static FirefoxProfile> {
        PROFILES.iter().find(|profile| profile.name == name)
    }

    /// Build the emulation for this release on the given platform.
    ///
    /// `http1_only` says that the client is restricted to HTTP/1.1. Firefox
    /// sends `TE: trailers` over HTTP/2 only, but the default headers cannot
    /// depend on the protocol a connection ends up with, so the header is
    /// left out only for such a client.
    pub fn emulation(&self, platform: Platform, http1_only: bool) -> wreq::Emulation {
        let base = BrowserEmulation::builder()
            .profile(BASE_PROFILE)
            .platform(platform)
            .build()
            .into_emulation();

        let mut headers = base.headers;
        let user_agent = headers
            .get(USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.replace(BASE_VERSION, self.version))
            .and_then(|value| HeaderValue::from_str(&value).ok());
        if let Some(user_agent) = user_agent {
            headers.insert(USER_AGENT, user_agent);
        }
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static(ACCEPT_LANGUAGE_VALUE),
        );

        // HTTP/2 forbids the header and the HTTP/2 client drops it, so it only
        // reaches the wire over HTTP/1.1, where Firefox sends it.
        headers.insert(CONNECTION, HeaderValue::from_static("keep-alive"));
        if http1_only {
            headers.remove(TE);
        }

        let mut orig_headers = OrigHeaderMap::with_capacity(HEADER_ORDER.len());
        for name in HEADER_ORDER {
            orig_headers.insert(*name);
        }

        let mut builder = wreq::Emulation::builder()
            .headers(headers)
            .orig_headers(orig_headers);

        if let Some(mut tls) = base.tls_options {
            tls.cipher_list = Some(Cow::Borrowed(self.cipher_list));
            tls.curves_list = Some(Cow::Borrowed(self.curves_list));
            builder = builder.tls_options(tls);
        }
        if let Some(http1) = base.http1_options {
            builder = builder.http1_options(http1);
        }
        if let Some(mut http2) = base.http2_options {
            http2.headers_stream_dependency = Some(StreamDependency::new(
                StreamId::zero(),
                HEADERS_PRIORITY_WEIGHT,
                false,
            ));
            builder = builder.http2_options(http2);
        }

        // A group of its own keeps pooled connections from being shared with
        // profiles whose TLS handshake differs.
        builder.build(Group::new(self.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header<'a>(emulation: &'a wreq::Emulation, name: &str) -> &'a str {
        emulation.headers.get(name).unwrap().to_str().unwrap()
    }

    #[test]
    fn covers_every_release_after_the_bundled_profiles() {
        for version in 152..=158 {
            let name = format!("firefox_{version}");
            let profile = FirefoxProfile::find(&name).unwrap_or_else(|| panic!("{name} missing"));
            assert_eq!(profile.version, format!("{version}.0"));
        }
        assert!(FirefoxProfile::find("firefox_151").is_none());
        assert!(FirefoxProfile::find("firefox_159").is_none());
    }

    #[test]
    fn names_are_unknown_to_wreq_util() {
        // A wreq-util profile of the same name takes precedence, so a hit here
        // means the local entry is dead: check upstream's profile and drop it.
        for profile in PROFILES {
            let name = serde_json::Value::String(profile.name.to_owned());
            assert!(
                serde_json::from_value::<BrowserProfile>(name).is_err(),
                "{} is now bundled with wreq-util",
                profile.name
            );
        }
    }

    #[test]
    fn headers_frame_priority_weight_is_corrected() {
        let expected = StreamDependency::new(StreamId::zero(), 41, false);
        for profile in PROFILES {
            let http2 = profile.emulation(Platform::default(), false).http2_options.unwrap();
            assert_eq!(http2.headers_stream_dependency, Some(expected), "{}", profile.name);
        }
    }

    #[test]
    fn user_agent_carries_the_version_on_every_platform() {
        let profile = FirefoxProfile::find("firefox_158").unwrap();
        for (platform, expected) in [
            (
                Platform::Windows,
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:158.0) Gecko/20100101 Firefox/158.0",
            ),
            (
                Platform::MacOS,
                "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:158.0) Gecko/20100101 Firefox/158.0",
            ),
            (
                Platform::Linux,
                "Mozilla/5.0 (X11; Ubuntu; Linux x86_64; rv:158.0) Gecko/20100101 Firefox/158.0",
            ),
            (
                Platform::Android,
                "Mozilla/5.0 (Android 13; Mobile; rv:158.0) Gecko/158.0 Firefox/158.0",
            ),
            (
                Platform::IOS,
                "Mozilla/5.0 (iPhone; CPU iPhone OS 18_2 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) FxiOS/158.0 Mobile/15E148 Safari/605.1.15",
            ),
        ] {
            let emulation = profile.emulation(platform, false);
            assert_eq!(header(&emulation, "user-agent"), expected);
            assert_eq!(header(&emulation, "accept-language"), "en-US,en;q=0.9");
        }
    }

    fn default_header_names(emulation: &wreq::Emulation) -> Vec<String> {
        emulation
            .headers
            .keys()
            .map(|name| name.as_str().to_owned())
            .collect()
    }

    #[test]
    fn default_headers_are_the_bundled_ones_plus_connection() {
        let base = BrowserEmulation::builder()
            .profile(BASE_PROFILE)
            .build()
            .into_emulation();
        let emulation = FirefoxProfile::find("firefox_158")
            .unwrap()
            .emulation(Platform::default(), false);

        let mut expected = default_header_names(&base);
        expected.push("connection".to_owned());
        assert_eq!(default_header_names(&emulation), expected);
        assert_eq!(header(&emulation, "connection"), "keep-alive");
    }

    /// Header names of the requests recorded from Firefox 152 and 158 over
    /// HTTP/1.1, merged into one sequence, plus `TE`, which only the HTTP/2
    /// recordings carry, and the four names whose place comes from `fetch()`
    /// requests and the Firefox sources. Kept apart from `HEADER_ORDER` on
    /// purpose, so that an edit there has to be made twice.
    const RECORDED_ORDER: [&str; 26] = [
        "Host", "User-Agent", "Accept", "Accept-Language", "Accept-Encoding", "Authorization",
        "Range", "Content-Type", "Content-Length", "Origin", "DNT", "Sec-GPC", "Connection",
        "Referer", "Cookie", "Upgrade-Insecure-Requests", "Sec-Fetch-Dest", "Sec-Fetch-Mode",
        "Sec-Fetch-Site", "Sec-Fetch-User", "If-Modified-Since", "If-None-Match", "Priority",
        "Pragma", "Cache-Control", "TE",
    ];

    /// The requests as recorded, each a subsequence of the order.
    const RECORDED_REQUESTS: [&[&str]; 5] = [
        // plain navigation
        &["Host", "User-Agent", "Accept", "Accept-Language", "Accept-Encoding", "Connection",
          "Upgrade-Insecure-Requests", "Sec-Fetch-Dest", "Sec-Fetch-Mode", "Sec-Fetch-Site",
          "Sec-Fetch-User", "Priority"],
        // repeat navigation with a cookie and cache validators
        &["Host", "User-Agent", "Accept", "Accept-Language", "Accept-Encoding", "Connection",
          "Cookie", "Upgrade-Insecure-Requests", "Sec-Fetch-Dest", "Sec-Fetch-Mode",
          "Sec-Fetch-Site", "Sec-Fetch-User", "If-Modified-Since", "If-None-Match", "Priority"],
        // reload
        &["Host", "User-Agent", "Accept", "Accept-Language", "Accept-Encoding", "Connection",
          "Cookie", "Upgrade-Insecure-Requests", "Sec-Fetch-Dest", "Sec-Fetch-Mode",
          "Sec-Fetch-Site", "Priority", "Pragma", "Cache-Control"],
        // link with a referrer
        &["Host", "User-Agent", "Accept", "Accept-Language", "Accept-Encoding", "Connection",
          "Referer", "Cookie", "Upgrade-Insecure-Requests", "Sec-Fetch-Dest", "Sec-Fetch-Mode",
          "Sec-Fetch-Site", "Priority"],
        // form POST
        &["Host", "User-Agent", "Accept", "Accept-Language", "Accept-Encoding", "Content-Type",
          "Content-Length", "Origin", "Connection", "Referer", "Cookie",
          "Upgrade-Insecure-Requests", "Sec-Fetch-Dest", "Sec-Fetch-Mode", "Sec-Fetch-Site",
          "Priority"],
    ];

    #[test]
    fn header_order_agrees_with_every_recorded_request() {
        for request in RECORDED_REQUESTS {
            let positions = request
                .iter()
                .map(|name| {
                    HEADER_ORDER
                        .iter()
                        .position(|listed| listed == name)
                        .unwrap_or_else(|| panic!("{name} is missing or spelled differently"))
                })
                .collect::<Vec<_>>();
            assert!(positions.windows(2).all(|pair| pair[0] < pair[1]), "{request:?}");
        }
    }

    #[test]
    fn unlisted_headers_go_where_a_script_sets_them() {
        let position = |name: &str| {
            HEADER_ORDER
                .iter()
                .position(|listed| listed.eq_ignore_ascii_case(name))
                .unwrap()
        };
        let slot = position(UNLISTED_HEADERS_BEFORE.as_str());
        assert!(position("accept-encoding") < slot);
        assert!(position("range") < slot);
        assert_eq!(HEADER_ORDER[slot], "Content-Type");
    }

    #[test]
    fn header_names_are_spelled_and_ordered_like_firefox() {
        for profile in PROFILES {
            let emulation = profile.emulation(Platform::default(), false);
            let spelled = emulation
                .orig_headers
                .iter()
                .map(|(_, name)| String::from_utf8(name.as_ref().to_vec()).unwrap())
                .collect::<Vec<_>>();
            assert_eq!(spelled, RECORDED_ORDER, "{}", profile.name);

            // A default header missing from the list would be written in lower
            // case after all the others.
            for name in emulation.headers.keys() {
                assert!(
                    HEADER_ORDER.iter().any(|listed| listed.eq_ignore_ascii_case(name.as_str())),
                    "{}: {name} has no place in HEADER_ORDER",
                    profile.name
                );
            }
        }
    }

    #[test]
    fn default_headers_already_follow_the_wire_order() {
        // The order list must not reorder the bundled headers, or the HTTP/2
        // request would change as well.
        let emulation = FirefoxProfile::find("firefox_158")
            .unwrap()
            .emulation(Platform::default(), false);
        let position = |name: &str| {
            HEADER_ORDER
                .iter()
                .position(|listed| listed.eq_ignore_ascii_case(name))
                .unwrap()
        };
        let bundled = default_header_names(&emulation)
            .into_iter()
            .filter(|name| name != "connection")
            .map(|name| position(&name))
            .collect::<Vec<_>>();
        assert!(bundled.windows(2).all(|pair| pair[0] < pair[1]), "{bundled:?}");
    }

    #[test]
    fn te_is_left_out_only_when_http1_is_certain() {
        let profile = FirefoxProfile::find("firefox_158").unwrap();
        let negotiated = profile.emulation(Platform::default(), false);
        assert_eq!(header(&negotiated, "te"), "trailers");

        let http1_only = profile.emulation(Platform::default(), true);
        assert!(http1_only.headers.get("te").is_none());
        assert_eq!(header(&http1_only, "connection"), "keep-alive");
    }

    #[test]
    fn tls_options_follow_the_release() {
        let tls = |name: &str| {
            let emulation = FirefoxProfile::find(name)
                .unwrap()
                .emulation(Platform::default(), false);
            let tls = emulation.tls_options.unwrap();
            (
                tls.cipher_list.unwrap().into_owned(),
                tls.curves_list.unwrap().into_owned(),
            )
        };
        const ECDSA_AES_128_CBC: &str = "TLS_ECDHE_ECDSA_WITH_AES_128_CBC_SHA";
        const ECDSA_AES_256_CBC: &str = "TLS_ECDHE_ECDSA_WITH_AES_256_CBC_SHA";

        for name in ["firefox_152", "firefox_153"] {
            let (ciphers, curves) = tls(name);
            assert_eq!(ciphers.split(':').count(), 16, "{name}");
            assert!(ciphers.contains(ECDSA_AES_256_CBC), "{name}");
            assert!(!ciphers.contains(ECDSA_AES_128_CBC), "{name}");
            assert!(curves.ends_with(":ffdhe2048:ffdhe3072"), "{name}");
        }
        for name in ["firefox_154", "firefox_155"] {
            let (ciphers, curves) = tls(name);
            assert_eq!(ciphers.split(':').count(), 15, "{name}");
            assert!(!ciphers.contains(ECDSA_AES_256_CBC), "{name}");
            assert!(curves.ends_with(":ffdhe2048:ffdhe3072"), "{name}");
        }
        for name in ["firefox_156", "firefox_157", "firefox_158"] {
            let (ciphers, curves) = tls(name);
            assert_eq!(ciphers.split(':').count(), 15, "{name}");
            assert!(!ciphers.contains(ECDSA_AES_256_CBC), "{name}");
            assert_eq!(curves, "X25519MLKEM768:X25519:P-256:P-384:P-521", "{name}");
        }
    }
}
