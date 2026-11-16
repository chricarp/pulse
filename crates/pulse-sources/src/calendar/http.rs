//! Fetching a calendar URL, and how far a feed may lead the daemon.
//!
//! A feed is someone else's server. It may answer with redirects, and its name may resolve to
//! anything, so the policy is enforced on the addresses each connection actually goes to (in the
//! resolver), not on the text of a `Location` header:
//! - The configured URL may be on a local or private address (a calendar on the user's NAS):
//!   whether it is, is decided the first time its host is resolved, and kept. A host that was
//!   public and later resolves to a local address (DNS rebinding) is refused.
//! - A redirect from a public feed may not lead to a loopback, private, link-local, unique-local,
//!   shared (CGNAT), multicast or unspecified address; such addresses are dropped from what a
//!   redirect target resolves to, and if none are left the fetch fails and says why.
//! - A redirect from https may not go to http. At most [`MAX_REDIRECTS`] redirects; the whole
//!   fetch, redirects included, within the timeout; the body within [`MAX_ICS_BYTES`].
//! - No proxy from the environment: through a proxy, the proxy resolves the names, and none of
//!   the above could be checked.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, OnceLock};

use ureq::config::Config;
use ureq::http::Uri;
use ureq::unversioned::resolver::{DefaultResolver, ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

use super::{Fetched, MAX_ICS_BYTES};

pub(super) const MAX_REDIRECTS: u32 = 5;

/// What one calendar source's fetches may reach. Kept for the source's life, so a decision about
/// the configured host outlives one fetch.
#[derive(Clone)]
pub(super) struct UrlPolicy {
    /// Whether the configured URL's host is local, from its first resolution.
    configured_local: Arc<OnceLock<bool>>,
    /// Tests use a stand-in: every server they can start is on 127.0.0.1.
    is_local: Arc<dyn Fn(&SocketAddr) -> bool + Send + Sync>,
}

impl fmt::Debug for UrlPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UrlPolicy").field("configured_local", &self.configured_local).finish()
    }
}

impl Default for UrlPolicy {
    fn default() -> Self {
        UrlPolicy { configured_local: Arc::default(), is_local: Arc::new(|a| is_local(a.ip())) }
    }
}

impl UrlPolicy {
    #[cfg(test)]
    pub(super) fn with_local(
        is_local: impl Fn(&SocketAddr) -> bool + Send + Sync + 'static,
    ) -> Self {
        UrlPolicy { configured_local: Arc::default(), is_local: Arc::new(is_local) }
    }
}

/// Addresses a feed on the internet has no business making the daemon connect to.
pub(super) fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => {
            let [o0, o1, ..] = a.octets();
            a.is_loopback()
                || a.is_private()
                || a.is_link_local()
                || a.is_unspecified()
                || a.is_broadcast()
                || a.is_multicast()
                || o0 == 0
                // 100.64.0.0/10, shared address space (CGNAT, Tailscale).
                || (o0 == 100 && (o1 & 0xc0) == 64)
        }
        IpAddr::V6(a) => {
            let s0 = a.segments()[0];
            a.is_loopback()
                || a.is_unspecified()
                || a.is_multicast()
                || (s0 & 0xfe00) == 0xfc00
                || (s0 & 0xffc0) == 0xfe80
                || a.to_ipv4_mapped().is_some_and(|v4| is_local(IpAddr::V4(v4)))
        }
    }
}

/// The resolver every connection of a fetch goes through.
struct Guard {
    inner: DefaultResolver,
    host: String,
    https: bool,
    policy: UrlPolicy,
}

impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guard").field("host", &self.host).finish_non_exhaustive()
    }
}

fn refused(message: String) -> ureq::Error {
    ureq::Error::Io(std::io::Error::new(std::io::ErrorKind::PermissionDenied, message))
}

impl Resolver for Guard {
    fn resolve(
        &self,
        uri: &Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let host = uri.host().unwrap_or_default();
        if self.https && uri.scheme_str() != Some("https") {
            return Err(refused(format!("a redirect from https to {uri} is not followed")));
        }
        let addrs = self.inner.resolve(uri, config, timeout)?;
        let is_local = &self.policy.is_local;
        let configured = host.eq_ignore_ascii_case(&self.host);
        let local_ok = if configured {
            *self.policy.configured_local.get_or_init(|| addrs.iter().any(|a| is_local(a)))
        } else {
            self.policy.configured_local.get() == Some(&true)
        };
        if local_ok {
            return Ok(addrs);
        }
        let mut public = self.empty();
        for a in addrs.iter().filter(|a| !is_local(a)) {
            public.push(*a);
        }
        if public.is_empty() {
            let what = if configured { "now resolves" } else { "is a redirect" };
            return Err(refused(format!(
                "{host} {what} to a local or private address ({}); a calendar feed may not \
                 lead there",
                addrs.first().map(|a| a.ip().to_string()).unwrap_or_default()
            )));
        }
        Ok(public)
    }
}

/// One HTTP GET with conditional headers, a size cap, a timeout for the whole fetch, and the
/// address policy above. Blocking; run it off the async threads.
pub(super) fn fetch_url(
    url: &str,
    etag: Option<&str>,
    last_modified: Option<&str>,
    timeout: std::time::Duration,
    policy: &UrlPolicy,
) -> Result<Fetched, String> {
    let uri: Uri = url.parse().map_err(|e| format!("cannot fetch {url}: {e}"))?;
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .max_redirects(MAX_REDIRECTS)
        .proxy(None)
        .user_agent(concat!("pulse/", env!("CARGO_PKG_VERSION")))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    let guard = Guard {
        inner: DefaultResolver::default(),
        host: uri.host().unwrap_or_default().to_owned(),
        https: uri.scheme_str() == Some("https"),
        policy: policy.clone(),
    };
    let agent = ureq::Agent::with_parts(config, DefaultConnector::default(), guard);
    let mut req = agent.get(url);
    if let Some(e) = etag {
        req = req.header("If-None-Match", e);
    }
    if let Some(m) = last_modified {
        req = req.header("If-Modified-Since", m);
    }
    let mut resp = req.call().map_err(|e| format!("cannot fetch {url}: {e}"))?;
    let status = resp.status().as_u16();
    if status == 304 {
        return Ok(Fetched::Unchanged);
    }
    if !(200..300).contains(&status) {
        return Err(format!("cannot fetch {url}: HTTP {status}"));
    }
    let header = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.chars().take(256).collect::<String>())
    };
    let (etag, last_modified) = (header("etag"), header("last-modified"));
    let text = resp
        .body_mut()
        .with_config()
        .limit(MAX_ICS_BYTES)
        .read_to_string()
        .map_err(|e| format!("cannot read {url}: {e} (the limit is {MAX_ICS_BYTES} bytes)"))?;
    Ok(Fetched::Body { text, etag, last_modified })
}
