use std::ffi::c_void;
use std::panic::{self, AssertUnwindSafe};
use std::ptr;
use std::any::Any;
use std::sync::Arc;
use std::time::Duration;

use magnus::{
    function, method, prelude::*, Module, RArray, RHash, Ruby,
    try_convert::TryConvert, Value,
};
use tokio::runtime::Runtime;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use std::net::IpAddr;
use wreq::header::{HeaderMap, HeaderName, HeaderValue, OrigHeaderMap};
use wreq::tls::TlsVersion;
use wreq::IntoEmulation;
use wreq_util::{Emulation as BrowserEmulation, Platform as EmulationPlatform, Profile as BrowserProfile};

use crate::error::{generic_error, to_magnus_error, wreq_error};
use crate::firefox::FirefoxProfile;
use crate::response::Response;

// --------------------------------------------------------------------------
// Shared Tokio runtime
// --------------------------------------------------------------------------

fn runtime() -> &'static Runtime {
    use std::sync::OnceLock;
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

// --------------------------------------------------------------------------
// GVL release helper
// --------------------------------------------------------------------------

/// Run a closure without the Ruby GVL, allowing other Ruby threads to execute.
/// The closure receives a `CancellationToken` that is cancelled if Ruby
/// interrupts the thread (e.g. `Thread.kill`, signal, timeout).
///
/// # Safety
/// The closure must NOT access any Ruby objects or call any Ruby C API.
/// Extract all data from Ruby before calling this, convert results after.
unsafe fn without_gvl<F, R>(f: F) -> Result<R, magnus::Error>
where
    F: FnOnce(CancellationToken) -> R,
{
    struct CallData<F, R> {
        func: Option<F>,
        result: Option<R>,
        token: CancellationToken,
        panic_payload: Option<Box<dyn Any + Send>>,
    }

    unsafe extern "C" fn call<F, R>(data: *mut c_void) -> *mut c_void
    where
        F: FnOnce(CancellationToken) -> R,
    {
        let d = data as *mut CallData<F, R>;
        let f = (*d).func.take().unwrap();
        let token = (*d).token.clone();
        // catch_unwind prevents a panic from unwinding through C frames (UB).
        match panic::catch_unwind(AssertUnwindSafe(|| f(token))) {
            Ok(val) => ptr::write(&mut (*d).result, Some(val)),
            Err(payload) => (*d).panic_payload = Some(payload),
        }
        ptr::null_mut()
    }

    /// Unblock function called by Ruby when it wants to interrupt this thread.
    /// Cancels the token so the in-flight async work can abort promptly.
    unsafe extern "C" fn ubf<F, R>(data: *mut c_void) {
        let d = data as *const CallData<F, R>;
        (*d).token.cancel();
    }

    let mut data = CallData {
        func: Some(f),
        result: None,
        token: CancellationToken::new(),
        panic_payload: None,
    };
    let data_ptr = &mut data as *mut CallData<F, R> as *mut c_void;

    unsafe {
        magnus::rb_sys::protect(|| {
            rb_sys::rb_thread_call_without_gvl(
                Some(call::<F, R>),
                data_ptr,
                Some(ubf::<F, R>),
                data_ptr,
            );
            0
        })?;
    }

    if let Some(payload) = data.panic_payload {
        panic::resume_unwind(payload);
    }

    Ok(data.result.unwrap())
}

/// Collected response data as pure Rust types (no Ruby objects).
struct ResponseData {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    url: String,
    version: String,
    content_length: Option<u64>,
    transfer_size: Option<u64>,
}

/// Outcome of the network call performed outside the GVL.
enum RequestOutcome {
    Ok(ResponseData),
    Err(wreq::Error),
    Interrupted,
}

/// Execute a request and collect the full response as pure Rust types.
async fn execute_request(req: wreq::RequestBuilder) -> Result<ResponseData, wreq::Error> {
    let resp = req.send().await?;
    let status = resp.status().as_u16();
    let url = resp.uri().to_string();
    let version = format!("{:?}", resp.version());
    let content_length = resp.content_length();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
        .collect();
    let transfer_size_handle = resp.transfer_size_handle().cloned();
    let body = resp.bytes().await?.to_vec();
    let transfer_size = transfer_size_handle.map(|h| h.get());
    Ok(ResponseData { status, headers, body, url, version, content_length, transfer_size })
}

// --------------------------------------------------------------------------
// Batch execution
// --------------------------------------------------------------------------

const DEFAULT_BATCH_CONCURRENCY: usize = 16;

/// Per-item batch result as pure Rust types (no Ruby objects). Errors are kept
/// as messages so one failure never discards the rest of the batch.
enum BatchItem {
    Ok(ResponseData),
    Err(String),
}

/// Outcome of a whole batch performed outside the GVL.
enum BatchOutcome {
    Done(Vec<BatchItem>),
    Interrupted,
}

/// Run every request concurrently with at most `concurrency` in flight,
/// returning results in input order.
async fn execute_batch(reqs: Vec<wreq::RequestBuilder>, concurrency: usize) -> Vec<BatchItem> {
    let permits = Arc::new(Semaphore::new(concurrency));
    let mut set: JoinSet<(usize, BatchItem)> = JoinSet::new();

    for (idx, req) in reqs.into_iter().enumerate() {
        let permits = Arc::clone(&permits);
        set.spawn(async move {
            let _permit = match permits.acquire_owned().await {
                Ok(p) => p,
                Err(_) => return (idx, BatchItem::Err("batch semaphore closed".to_owned())),
            };
            let item = match execute_request(req).await {
                Ok(data) => BatchItem::Ok(data),
                Err(e) => BatchItem::Err(e.to_string()),
            };
            (idx, item)
        });
    }

    let mut slots: Vec<Option<BatchItem>> = Vec::new();
    slots.resize_with(set.len(), || None);
    while let Some(joined) = set.join_next().await {
        // A JoinError means the task panicked or was aborted; its slot is left
        // empty and filled with a generic error below.
        if let Ok((idx, item)) = joined {
            slots[idx] = Some(item);
        }
    }

    slots
        .into_iter()
        .map(|slot| slot.unwrap_or_else(|| BatchItem::Err("request task failed".to_owned())))
        .collect()
}

// --------------------------------------------------------------------------
// Emulation helpers
// --------------------------------------------------------------------------

/// The default browser profile to apply when none is specified.
const DEFAULT_EMULATION: EmulationProfile = EmulationProfile::Bundled(BrowserProfile::Chrome149);

/// A browser profile selectable through the `emulation` option.
#[derive(Clone, Copy)]
enum EmulationProfile {
    /// A profile that ships with wreq-util.
    Bundled(BrowserProfile),
    /// A Firefox release newer than wreq-util knows about.
    Firefox(&'static FirefoxProfile),
}

/// Parse a Ruby string like "chrome_143" into an emulation profile.
fn parse_emulation(name: &str) -> Result<EmulationProfile, magnus::Error> {
    let json_val = serde_json::Value::String(name.to_string());
    if let Ok(profile) = serde_json::from_value::<BrowserProfile>(json_val) {
        return Ok(EmulationProfile::Bundled(profile));
    }
    FirefoxProfile::find(name)
        .map(EmulationProfile::Firefox)
        .ok_or_else(|| generic_error(format!("unknown emulation: '{}'. Use names like 'chrome_148', 'firefox_158', 'safari_18.5', etc.", name)))
}

/// Parse a Ruby string like "windows" into an EmulationPlatform variant.
fn parse_emulation_os(name: &str) -> Result<EmulationPlatform, magnus::Error> {
    let json_val = serde_json::Value::String(name.to_string());
    serde_json::from_value::<EmulationPlatform>(json_val)
        .map_err(|_| generic_error("unknown emulation_os. Use: 'windows', 'macos', 'linux', 'android', 'ios'"))
}

/// Build the emulation for a profile on the given platform. `http1_only` says
/// that the client never negotiates HTTP/2.
fn build_emulation(
    profile: EmulationProfile,
    platform: EmulationPlatform,
    http1_only: bool,
) -> wreq::Emulation {
    match profile {
        EmulationProfile::Bundled(profile) => BrowserEmulation::builder()
            .profile(profile)
            .platform(platform)
            .build()
            .into_emulation(),
        EmulationProfile::Firefox(profile) => profile.emulation(platform, http1_only),
    }
}

/// Build the emulation for a profile and an optional platform from the opts hash.
fn build_emulation_option(
    profile: EmulationProfile,
    opts: &RHash,
    http1_only: bool,
) -> Result<wreq::Emulation, magnus::Error> {
    let platform = match hash_get_string(opts, "emulation_os")? {
        Some(platform_name) => parse_emulation_os(&platform_name)?,
        None => EmulationPlatform::default(),
    };
    Ok(build_emulation(profile, platform, http1_only))
}

/// The header order of a client or request whose profile spells and orders the
/// request headers itself, which today means the local Firefox profiles.
///
/// wreq writes the headers such an order names first and whatever is left
/// after them, in an order that follows from how its header map removes
/// entries rather than from the order the headers were given in. So the order
/// handed to wreq names every header the gem knows the request carries.
#[derive(Clone)]
struct HeaderOrder {
    /// Names fixed by the `header_order` option. They come first, spelled as
    /// the user spelled them.
    user: OrigHeaderMap,
    /// The profile's names, without the ones in `user`.
    profile: OrigHeaderMap,
    /// The profile name that headers neither list names are written before:
    /// the profile's own slot for them, or the next name after it that
    /// `user` left in `profile`. `None` puts them last.
    slot: Option<HeaderName>,
    /// The names in the client's `headers` option, spelled as given. Those
    /// that neither list names get a place.
    extra: Vec<String>,
}

impl HeaderOrder {
    fn new(user: Option<&OrigHeaderMap>, profile: &OrigHeaderMap, extra: Vec<String>) -> Self {
        let user = user.cloned().unwrap_or_default();
        let mut rest = OrigHeaderMap::with_capacity(profile.len());
        let mut slot = None;
        let mut slot_reached = false;
        for (name, spelled) in profile.iter() {
            slot_reached |= *name == crate::firefox::UNLISTED_HEADERS_BEFORE;
            if !user.iter().any(|(ordered, _)| ordered == name) {
                rest.insert(spelled);
                if slot_reached && slot.is_none() {
                    slot = Some(name.clone());
                }
            }
        }
        HeaderOrder { user, profile: rest, slot, extra }
    }

    fn lists(&self, name: &HeaderName) -> bool {
        self.user.iter().chain(self.profile.iter()).any(|(listed, _)| listed == name)
    }

    /// The headers among the client's and the request's own that the order
    /// has no place for, in the order they were given.
    fn unlisted<'a>(&'a self, request: &'a [String]) -> Vec<(HeaderName, &'a str)> {
        let mut unlisted: Vec<(HeaderName, &str)> = Vec::new();
        for spelled in self.extra.iter().chain(request) {
            let Ok(name) = HeaderName::from_bytes(spelled.as_bytes()) else {
                continue;
            };
            if !self.lists(&name) && !unlisted.iter().any(|(seen, _)| *seen == name) {
                unlisted.push((name, spelled));
            }
        }
        unlisted
    }

    /// Whether a request with these headers needs an order of its own, that
    /// is whether it carries a header the client's order does not name.
    fn misses(&self, request: &[String]) -> bool {
        self.unlisted(request).len() > self.unlisted(&[]).len()
    }

    /// The order for a request that carries the `request` headers on top of
    /// the client's.
    fn with(&self, request: &[String]) -> OrigHeaderMap {
        let unlisted = self.unlisted(request);
        let mut order = OrigHeaderMap::with_capacity(
            self.user.len() + self.profile.len() + unlisted.len(),
        );
        for (_, spelled) in self.user.iter() {
            order.insert(spelled);
        }

        let mut placed = false;
        for (name, spelled) in self.profile.iter() {
            if !placed && self.slot.as_ref() == Some(name) {
                for (_, spelled) in &unlisted {
                    order.insert(spelled.to_string());
                }
                placed = true;
            }
            order.insert(spelled);
        }
        if !placed {
            for (_, spelled) in &unlisted {
                order.insert(spelled.to_string());
            }
        }
        order
    }
}

/// Whether a client with these options only ever speaks HTTP/1.1. Both options
/// together leave an HTTP/2 client: `http2_only` is applied last and wins.
fn restricted_to_http1(http1_only: bool, http2_only: bool) -> bool {
    http1_only && !http2_only
}

/// What applying a request's options needs to know about the client, and what
/// it learns about the request's headers on the way.
struct RequestHeaders {
    http1_only: bool,
    /// The header order in force: the client's, or the one of a profile
    /// selected for this request.
    order: Option<Arc<HeaderOrder>>,
    /// Headers the client adds to every request, spelled as given.
    client: Arc<Vec<String>>,
    /// Headers set for this request, spelled as given.
    request: Vec<String>,
}

impl RequestHeaders {
    /// Give the request an order of its own when it carries headers the
    /// order in force does not name.
    fn finish(self, req: wreq::RequestBuilder) -> wreq::RequestBuilder {
        match &self.order {
            Some(order) if order.misses(&self.request) => req.orig_headers(order.with(&self.request)),
            _ => req,
        }
    }
}

// --------------------------------------------------------------------------
// Ruby Client
// --------------------------------------------------------------------------

#[magnus::wrap(class = "Wreq::Client", free_immediately)]
struct Client {
    inner: wreq::Client,
    /// The client never negotiates HTTP/2 (`http1_only: true`).
    http1_only: bool,
    /// Set when the client's profile orders the request headers itself.
    header_order: Option<Arc<HeaderOrder>>,
    /// The names in the client's `headers` option, spelled as given.
    header_names: Arc<Vec<String>>,
    cancel_token: std::sync::Mutex<CancellationToken>,
}

impl Client {
    /// Wreq::Client.new or Wreq::Client.new(options_hash)
    fn rb_new(args: &[Value]) -> Result<Self, magnus::Error> {
        let opts: Option<RHash> = if args.is_empty() {
            None
        } else {
            Some(RHash::try_convert(args[0])?)
        };

        let mut builder = wreq::Client::builder()
            .retry(wreq::retry::Policy::never());

        let mut http1_only = false;
        let mut header_order = None;
        let mut header_names = Vec::new();

        if let Some(opts) = opts {
            let force_http1 = hash_get_bool(&opts, "http1_only")? == Some(true);
            let force_http2 = hash_get_bool(&opts, "http2_only")? == Some(true);
            http1_only = restricted_to_http1(force_http1, force_http2);

            let client_headers = match hash_get_hash(&opts, "headers")? {
                Some(hdr_hash) => Some(hash_to_named_header_map(&hdr_hash)?),
                None => None,
            };
            if let Some((_, names)) = &client_headers {
                header_names = names.clone();
            }

            // Apply header_order BEFORE emulation so the user's ordering takes precedence
            let user_order = match hash_get_array(&opts, "header_order")? {
                Some(ary) => Some(array_to_orig_header_map(ary)?),
                None => None,
            };
            if let Some(orig) = &user_order {
                builder = builder.orig_headers(orig.clone());
            }

            let profile = match hash_get_value(&opts, "emulation")? {
                Some(val) => {
                    let ruby = unsafe { Ruby::get_unchecked() };
                    if val.is_kind_of(ruby.class_false_class()) {
                        // emulation: false — skip emulation entirely
                        None
                    } else if val.is_kind_of(ruby.class_true_class()) {
                        Some(DEFAULT_EMULATION)
                    } else {
                        let name: String = TryConvert::try_convert(val)?;
                        Some(parse_emulation(&name)?)
                    }
                }
                None => Some(DEFAULT_EMULATION),
            };
            if let Some(profile) = profile {
                let mut emulation = build_emulation_option(profile, &opts, http1_only)?;
                if let EmulationProfile::Firefox(_) = profile {
                    // The user's position and spelling of a header win over
                    // the profile's, and the client's own headers get a place.
                    let order = HeaderOrder::new(
                        user_order.as_ref(),
                        &emulation.orig_headers,
                        header_names.clone(),
                    );
                    emulation.orig_headers = order.with(&[]);
                    header_order = Some(Arc::new(order));
                }
                builder = builder.emulation(emulation);
            }

            if let Some(ua) = hash_get_string(&opts, "user_agent")? {
                builder = builder.user_agent(ua);
            }

            if let Some((hmap, _)) = client_headers {
                builder = builder.default_headers(hmap);
            }

            let timeout = hash_get_float(&opts, "timeout")?;
            if let Some(timeout) = timeout {
                builder = builder.timeout(Duration::from_secs_f64(timeout));
            }

            if let Some(connect_timeout) = hash_get_float(&opts, "connect_timeout")?.or(timeout) {
                builder = builder.connect_timeout(Duration::from_secs_f64(connect_timeout));
            }

            if let Some(t) = hash_get_float(&opts, "read_timeout")? {
                builder = builder.read_timeout(Duration::from_secs_f64(t));
            }

            if let Some(val) = hash_get_value(&opts, "redirect")? {
                let ruby = unsafe { Ruby::get_unchecked() };
                if val.is_kind_of(ruby.class_false_class()) {
                    builder = builder.redirect(wreq::redirect::Policy::none());
                } else if val.is_kind_of(ruby.class_true_class()) {
                    builder = builder.redirect(wreq::redirect::Policy::limited(10));
                } else {
                    let n: usize = TryConvert::try_convert(val)?;
                    builder = builder.redirect(wreq::redirect::Policy::limited(n));
                }
            }

            if let Some(enabled) = hash_get_bool(&opts, "cookie_store")? {
                builder = builder.cookie_store(enabled);
            }

            if let Some(proxy_url) = hash_get_string(&opts, "proxy")? {
                let mut proxy = wreq::Proxy::all(&proxy_url).map_err(to_magnus_error)?;
                if let (Some(user), Some(pass)) = (
                    hash_get_string(&opts, "proxy_user")?,
                    hash_get_string(&opts, "proxy_pass")?,
                ) {
                    proxy = proxy.basic_auth(&user, &pass);
                }
                builder = builder.proxy(proxy);
            }

            if let Some(true) = hash_get_bool(&opts, "no_proxy")? {
                builder = builder.no_proxy();
            }

            if let Some(enabled) = hash_get_bool(&opts, "https_only")? {
                builder = builder.https_only(enabled);
            }

            if let Some(v) = hash_get_bool(&opts, "verify_host")? {
                builder = builder.tls_verify_hostname(v);
            }

            if let Some(v) = hash_get_bool(&opts, "verify_cert")? {
                builder = builder.tls_cert_verification(v);
            }

            if force_http1 {
                builder = builder.http1_only();
            }
            if force_http2 {
                builder = builder.http2_only();
            }

            if let Some(v) = hash_get_bool(&opts, "gzip")? {
                builder = builder.gzip(v);
            }
            if let Some(v) = hash_get_bool(&opts, "brotli")? {
                builder = builder.brotli(v);
            }
            if let Some(v) = hash_get_bool(&opts, "deflate")? {
                builder = builder.deflate(v);
            }
            if let Some(v) = hash_get_bool(&opts, "zstd")? {
                builder = builder.zstd(v);
            }

            if let Some(v) = hash_get_bool(&opts, "referer")? {
                builder = builder.referer(v);
            }

            if let Some(n) = hash_get_usize(&opts, "pool_max_idle_per_host")? {
                builder = builder.pool_max_idle_per_host(n);
            }

            if let Some(n) = hash_get_usize(&opts, "pool_max_size")? {
                builder = builder.pool_max_size(n);
            }

            if let Some(v) = hash_get_bool(&opts, "tcp_nodelay")? {
                builder = builder.tcp_nodelay(v);
            }

            if let Some(t) = hash_get_float(&opts, "tcp_keepalive")? {
                builder = builder.tcp_keepalive(Duration::from_secs_f64(t));
            }

            if let Some(addr_str) = hash_get_string(&opts, "local_address")? {
                let addr: IpAddr = addr_str.parse()
                    .map_err(|_| generic_error(format!("invalid IP address: '{}'", addr_str)))?;
                builder = builder.local_address(addr);
            }

            if let Some(v) = hash_get_bool(&opts, "tls_sni")? {
                builder = builder.tls_sni(v);
            }

            if let Some(s) = hash_get_string(&opts, "min_tls_version")? {
                builder = builder.tls_min_version(parse_tls_version(&s)?);
            }

            if let Some(s) = hash_get_string(&opts, "max_tls_version")? {
                builder = builder.tls_max_version(parse_tls_version(&s)?);
            }
        } else {
            builder = builder.emulation(build_emulation(DEFAULT_EMULATION, EmulationPlatform::default(), false));
        }

        let client = builder.build().map_err(to_magnus_error)?;
        Ok(Client {
            inner: client,
            http1_only,
            header_order,
            header_names: Arc::new(header_names),
            cancel_token: std::sync::Mutex::new(CancellationToken::new()),
        })
    }

    /// client.get(url) or client.get(url, opts)
    fn get(&self, args: &[Value]) -> Result<Response, magnus::Error> {
        self.execute_method("GET", args)
    }

    fn post(&self, args: &[Value]) -> Result<Response, magnus::Error> {
        self.execute_method("POST", args)
    }

    fn put(&self, args: &[Value]) -> Result<Response, magnus::Error> {
        self.execute_method("PUT", args)
    }

    fn patch(&self, args: &[Value]) -> Result<Response, magnus::Error> {
        self.execute_method("PATCH", args)
    }

    fn delete(&self, args: &[Value]) -> Result<Response, magnus::Error> {
        self.execute_method("DELETE", args)
    }

    fn head(&self, args: &[Value]) -> Result<Response, magnus::Error> {
        self.execute_method("HEAD", args)
    }

    fn options(&self, args: &[Value]) -> Result<Response, magnus::Error> {
        self.execute_method("OPTIONS", args)
    }

    fn cancel(&self) {
        // Replace the cancel token first so new requests use a fresh token,
        // then cancel the old one to unblock all current in-flight select!s.
        let old_token = {
            let mut guard = self.cancel_token.lock().unwrap_or_else(|e| e.into_inner());
            let old = guard.clone();
            *guard = CancellationToken::new();
            old
        };
        old_token.cancel();
    }

    fn execute_method(&self, method_str: &str, args: &[Value]) -> Result<Response, magnus::Error> {
        let url: String = if args.is_empty() {
            return Err(generic_error("url is required"));
        } else {
            TryConvert::try_convert(args[0])?
        };

        let opts: Option<RHash> = if args.len() > 1 {
            Some(RHash::try_convert(args[1])?)
        } else {
            None
        };

        let method: wreq::Method = method_str
            .parse()
            .map_err(|_| generic_error(format!("invalid HTTP method: {}", method_str)))?;

        let mut req = self.inner.request(method, &url);

        let mut headers = self.request_headers();
        if let Some(opts) = opts {
            req = apply_request_options(req, &opts, &mut headers)?;
        }
        req = headers.finish(req);

        let client_token = self.cancel_token.lock().unwrap_or_else(|e| e.into_inner()).clone();

        // Release the GVL so other Ruby threads can run during I/O.
        let outcome: RequestOutcome = unsafe {
            without_gvl(|thread_token| {
                runtime().block_on(async {
                    tokio::select! {
                        biased;
                        _ = thread_token.cancelled() => RequestOutcome::Interrupted,
                        _ = client_token.cancelled() => RequestOutcome::Interrupted,
                        res = execute_request(req) => match res {
                            Ok(data) => RequestOutcome::Ok(data),
                            Err(e) => RequestOutcome::Err(e),
                        },
                    }
                })
            })
        }?;

        let data = match outcome {
            RequestOutcome::Ok(d) => d,
            RequestOutcome::Err(e) => return Err(to_magnus_error(e)),
            RequestOutcome::Interrupted => return Err(generic_error("request interrupted")),
        };
        Ok(Response::new(data.status, data.headers, data.body, data.url, data.version, data.content_length, data.transfer_size))
    }

    /// Wreq::Client#request_batch(specs) or #request_batch(specs, options)
    fn request_batch(&self, args: &[Value]) -> Result<RArray, magnus::Error> {
        if args.is_empty() {
            return Err(generic_error("an array of requests is required"));
        }
        let specs = RArray::try_convert(args[0])?;

        let opts: Option<RHash> = if args.len() > 1 {
            Some(RHash::try_convert(args[1])?)
        } else {
            None
        };

        let concurrency = match opts.as_ref() {
            Some(o) => hash_get_usize(o, "concurrency")?.unwrap_or(DEFAULT_BATCH_CONCURRENCY),
            None => DEFAULT_BATCH_CONCURRENCY,
        };
        if concurrency == 0 {
            return Err(generic_error("concurrency must be >= 1"));
        }

        // All Ruby -> Rust conversion happens here, while we still hold the GVL.
        let mut reqs: Vec<wreq::RequestBuilder> = Vec::with_capacity(specs.len());
        for spec in specs.into_iter() {
            reqs.push(self.build_request(spec, opts.as_ref())?);
        }

        let ruby = unsafe { Ruby::get_unchecked() };
        if reqs.is_empty() {
            return Ok(ruby.ary_new());
        }

        let client_token = self.cancel_token.lock().unwrap_or_else(|e| e.into_inner()).clone();

        // One GVL release covering the whole batch.
        let outcome: BatchOutcome = unsafe {
            without_gvl(|thread_token| {
                runtime().block_on(async {
                    tokio::select! {
                        biased;
                        _ = thread_token.cancelled() => BatchOutcome::Interrupted,
                        _ = client_token.cancelled() => BatchOutcome::Interrupted,
                        items = execute_batch(reqs, concurrency) => BatchOutcome::Done(items),
                    }
                })
            })
        }?;

        let items = match outcome {
            BatchOutcome::Done(items) => items,
            BatchOutcome::Interrupted => return Err(generic_error("request interrupted")),
        };

        // Back under the GVL: Ruby objects may be created again.
        let results = ruby.ary_new_capa(items.len());
        for item in items {
            match item {
                BatchItem::Ok(d) => {
                    let resp = Response::new(
                        d.status, d.headers, d.body, d.url, d.version, d.content_length,
                        d.transfer_size,
                    );
                    results.push(ruby.obj_wrap(resp))?;
                }
                BatchItem::Err(msg) => {
                    let err: Value = wreq_error().funcall("new", (msg,))?;
                    results.push(err)?;
                }
            }
        }
        Ok(results)
    }

    /// Convert a single batch spec into a RequestBuilder. Accepted forms:
    ///   "https://example.com"      -> GET
    ///   { method:, url:, **opts }
    fn build_request(
        &self,
        spec: Value,
        shared: Option<&RHash>,
    ) -> Result<wreq::RequestBuilder, magnus::Error> {
        let mut method = wreq::Method::GET;
        let url: String;
        let mut item_opts: Option<RHash> = None;

        if let Some(hash) = RHash::from_value(spec) {
            url = hash_get_string(&hash, "url")?
                .ok_or_else(|| generic_error("each request hash requires a :url"))?;
            if let Some(val) = hash_get_value(&hash, "method")? {
                method = value_to_method(val)?;
            }
            item_opts = Some(hash);
        } else {
            url = TryConvert::try_convert(spec)?;
        }

        let mut req = self.inner.request(method, &url);
        let mut headers = self.request_headers();
        if let Some(shared) = shared {
            req = apply_request_options(req, shared, &mut headers)?;
        }
        if let Some(item_opts) = item_opts {
            req = apply_request_options(req, &item_opts, &mut headers)?;
        }
        Ok(headers.finish(req))
    }

    fn request_headers(&self) -> RequestHeaders {
        RequestHeaders {
            http1_only: self.http1_only,
            order: self.header_order.clone(),
            client: self.header_names.clone(),
            request: Vec::new(),
        }
    }
}

/// Parse a String or Symbol like "post" / :post into an HTTP method.
fn value_to_method(val: Value) -> Result<wreq::Method, magnus::Error> {
    let name: String = val.funcall("to_s", ())?;
    name.to_uppercase()
        .parse()
        .map_err(|_| generic_error(format!("invalid HTTP method: {}", name)))
}

fn apply_request_options(
    mut req: wreq::RequestBuilder,
    opts: &RHash,
    headers: &mut RequestHeaders,
) -> Result<wreq::RequestBuilder, magnus::Error> {
    if let Some(val) = hash_get_value(opts, "emulation")? {
        let ruby = unsafe { Ruby::get_unchecked() };
        if val.is_kind_of(ruby.class_false_class()) {
            // emulation: false — no per-request emulation override
        } else {
            let profile = if val.is_kind_of(ruby.class_true_class()) {
                DEFAULT_EMULATION
            } else {
                let name: String = TryConvert::try_convert(val)?;
                parse_emulation(&name)?
            };
            let mut opt = build_emulation_option(profile, opts, headers.http1_only)?;
            // A profile selected for the request replaces the client's header
            // order with its own, which is empty for the bundled profiles.
            headers.order = match profile {
                EmulationProfile::Firefox(_) => {
                    // The client's own headers still go out with the request.
                    let order = HeaderOrder::new(None, &opt.orig_headers, headers.client.to_vec());
                    opt.orig_headers = order.with(&[]);
                    Some(Arc::new(order))
                }
                EmulationProfile::Bundled(_) => None,
            };
            req = req.emulation(opt);
        }
    }

    if let Some(hdr_hash) = hash_get_hash(opts, "headers")? {
        let (hmap, names) = hash_to_named_header_map(&hdr_hash)?;
        headers.request.extend(names);
        req = req.headers(hmap);
    }

    if let Some(body_str) = hash_get_string(opts, "body")? {
        req = req.body(body_str);
    }

    if let Some(json_val) = hash_get_value(opts, "json")? {
        let ruby = unsafe { Ruby::get_unchecked() };
        let json_module: Value = ruby.class_object().const_get("JSON")?;
        let json_str: String = json_module.funcall("generate", (json_val,))?;
        req = req
            .header("content-type", "application/json")
            .body(json_str);
    }

    if let Some(form_hash) = hash_get_hash(opts, "form")? {
        let pairs = hash_to_pairs(&form_hash)?;
        req = req.form(&pairs);
    }

    if let Some(query_hash) = hash_get_hash(opts, "query")? {
        let pairs = hash_to_pairs(&query_hash)?;
        req = req.query(&pairs);
    }

    if let Some(t) = hash_get_float(opts, "timeout")? {
        req = req.timeout(Duration::from_secs_f64(t));
    }

    if let Some(token) = hash_get_string(opts, "auth")? {
        req = req.auth(token);
    }

    if let Some(token) = hash_get_string(opts, "bearer")? {
        req = req.bearer_auth(token);
    }

    if let Some(basic_val) = hash_get_value(opts, "basic")? {
        let ary = RArray::try_convert(basic_val)?;
        if ary.len() >= 2 {
            let user: String = TryConvert::try_convert(ary.entry::<Value>(0)?)?;
            let pass: String = TryConvert::try_convert(ary.entry::<Value>(1)?)?;
            req = req.basic_auth(user, Some(pass));
        }
    }

    if let Some(proxy_url) = hash_get_string(opts, "proxy")? {
        let proxy = wreq::Proxy::all(&proxy_url).map_err(to_magnus_error)?;
        req = req.proxy(proxy);
    }

    Ok(req)
}

// --------------------------------------------------------------------------
// Module-level convenience methods
// --------------------------------------------------------------------------

fn wreq_get(args: &[Value]) -> Result<Response, magnus::Error> {
    let client = Client::rb_new(&[])?;
    client.execute_method("GET", args)
}

fn wreq_post(args: &[Value]) -> Result<Response, magnus::Error> {
    let client = Client::rb_new(&[])?;
    client.execute_method("POST", args)
}

fn wreq_put(args: &[Value]) -> Result<Response, magnus::Error> {
    let client = Client::rb_new(&[])?;
    client.execute_method("PUT", args)
}

fn wreq_patch(args: &[Value]) -> Result<Response, magnus::Error> {
    let client = Client::rb_new(&[])?;
    client.execute_method("PATCH", args)
}

fn wreq_delete(args: &[Value]) -> Result<Response, magnus::Error> {
    let client = Client::rb_new(&[])?;
    client.execute_method("DELETE", args)
}

fn wreq_head(args: &[Value]) -> Result<Response, magnus::Error> {
    let client = Client::rb_new(&[])?;
    client.execute_method("HEAD", args)
}

// --------------------------------------------------------------------------
// Hash helpers
// --------------------------------------------------------------------------

fn hash_get_value(hash: &RHash, key: &str) -> Result<Option<Value>, magnus::Error> {
    // Try string key
    let val: Value = hash.aref(key)?;
    if !val.is_nil() {
        return Ok(Some(val));
    }
    // Try symbol key
    let ruby = unsafe { Ruby::get_unchecked() };
    let sym = ruby.to_symbol(key);
    let val: Value = hash.aref(sym)?;
    if !val.is_nil() {
        return Ok(Some(val));
    }
    Ok(None)
}

fn hash_get_string(hash: &RHash, key: &str) -> Result<Option<String>, magnus::Error> {
    match hash_get_value(hash, key)? {
        Some(v) => Ok(Some(TryConvert::try_convert(v)?)),
        None => Ok(None),
    }
}

fn hash_get_float(hash: &RHash, key: &str) -> Result<Option<f64>, magnus::Error> {
    match hash_get_value(hash, key)? {
        Some(v) => Ok(Some(TryConvert::try_convert(v)?)),
        None => Ok(None),
    }
}

fn hash_get_bool(hash: &RHash, key: &str) -> Result<Option<bool>, magnus::Error> {
    match hash_get_value(hash, key)? {
        Some(v) => Ok(Some(TryConvert::try_convert(v)?)),
        None => Ok(None),
    }
}

fn hash_get_usize(hash: &RHash, key: &str) -> Result<Option<usize>, magnus::Error> {
    match hash_get_value(hash, key)? {
        Some(v) => Ok(Some(TryConvert::try_convert(v)?)),
        None => Ok(None),
    }
}

fn parse_tls_version(s: &str) -> Result<TlsVersion, magnus::Error> {
    match s {
        "tls1.0" | "tls_1_0" | "1.0" => Ok(TlsVersion::TLS_1_0),
        "tls1.1" | "tls_1_1" | "1.1" => Ok(TlsVersion::TLS_1_1),
        "tls1.2" | "tls_1_2" | "1.2" => Ok(TlsVersion::TLS_1_2),
        "tls1.3" | "tls_1_3" | "1.3" => Ok(TlsVersion::TLS_1_3),
        _ => Err(generic_error(format!(
            "unknown TLS version '{}'. Use: 'tls1.2', 'tls1.3'", s
        ))),
    }
}

fn hash_get_hash(hash: &RHash, key: &str) -> Result<Option<RHash>, magnus::Error> {
    match hash_get_value(hash, key)? {
        Some(v) => Ok(Some(RHash::try_convert(v)?)),
        None => Ok(None),
    }
}

fn hash_get_array(hash: &RHash, key: &str) -> Result<Option<RArray>, magnus::Error> {
    match hash_get_value(hash, key)? {
        Some(v) => Ok(Some(RArray::try_convert(v)?)),
        None => Ok(None),
    }
}

fn array_to_orig_header_map(ary: RArray) -> Result<OrigHeaderMap, magnus::Error> {
    let mut orig = OrigHeaderMap::with_capacity(ary.len());
    for elem in ary.into_iter() {
        let ruby = unsafe { Ruby::get_unchecked() };
        let name_str: String = if elem.is_kind_of(ruby.class_symbol()) {
            elem.funcall("to_s", ())?  
        } else {
            TryConvert::try_convert(elem)?
        };
        orig.insert(name_str);
    }
    Ok(orig)
}

/// Convert a headers hash into a header map, along with the names as the
/// caller spelled them, in the order given.
fn hash_to_named_header_map(hash: &RHash) -> Result<(HeaderMap, Vec<String>), magnus::Error> {
    let mut hmap = HeaderMap::new();
    let mut names = Vec::new();
    hash.foreach(|k: Value, v: Value| {
        if v.is_nil() {
            return Ok(magnus::r_hash::ForEach::Continue);
        }
        let ruby = unsafe { Ruby::get_unchecked() };
        let ks: String = if k.is_kind_of(ruby.class_symbol()) {
            k.funcall("to_s", ())?
        } else {
            TryConvert::try_convert(k)?
        };
        let vs: String = v.funcall("to_s", ())?;
        let name =
            HeaderName::from_bytes(ks.as_bytes()).map_err(|e| generic_error(e))?;
        let value = HeaderValue::from_str(&vs).map_err(|e| generic_error(e))?;
        hmap.insert(name, value);
        names.push(ks);
        Ok(magnus::r_hash::ForEach::Continue)
    })?;
    Ok((hmap, names))
}

fn hash_to_pairs(hash: &RHash) -> Result<Vec<(String, String)>, magnus::Error> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    hash.foreach(|k: Value, v: Value| {
        let ruby = unsafe { Ruby::get_unchecked() };
        let ks: String = if k.is_kind_of(ruby.class_symbol()) {
            k.funcall("to_s", ())?
        } else {
            TryConvert::try_convert(k)?
        };
        let vs: String = v.funcall("to_s", ())?;
        pairs.push((ks, vs));
        Ok(magnus::r_hash::ForEach::Continue)
    })?;
    Ok(pairs)
}

// --------------------------------------------------------------------------
// Init
// --------------------------------------------------------------------------

pub fn init(_ruby: &magnus::Ruby, module: &magnus::RModule) -> Result<(), magnus::Error> {
    let ruby = unsafe { Ruby::get_unchecked() };
    let client_class = module.define_class("Client", ruby.class_object())?;
    client_class.define_singleton_method("new", function!(Client::rb_new, -1))?;
    client_class.define_method("get", method!(Client::get, -1))?;
    client_class.define_method("post", method!(Client::post, -1))?;
    client_class.define_method("put", method!(Client::put, -1))?;
    client_class.define_method("patch", method!(Client::patch, -1))?;
    client_class.define_method("delete", method!(Client::delete, -1))?;
    client_class.define_method("head", method!(Client::head, -1))?;
    client_class.define_method("options", method!(Client::options, -1))?;
    client_class.define_method("request_batch", method!(Client::request_batch, -1))?;
    client_class.define_method("cancel", method!(Client::cancel, 0))?;

    module.define_module_function("get", function!(wreq_get, -1))?;
    module.define_module_function("post", function!(wreq_post, -1))?;
    module.define_module_function("put", function!(wreq_put, -1))?;
    module.define_module_function("patch", function!(wreq_patch, -1))?;
    module.define_module_function("delete", function!(wreq_delete, -1))?;
    module.define_module_function("head", function!(wreq_head, -1))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn firefox_order() -> OrigHeaderMap {
        FirefoxProfile::find("firefox_158")
            .unwrap()
            .emulation(EmulationPlatform::default(), false)
            .orig_headers
    }

    fn order_of(names: &[&'static str]) -> OrigHeaderMap {
        let mut order = OrigHeaderMap::new();
        for name in names {
            order.insert(*name);
        }
        order
    }

    fn spelled(order: &OrigHeaderMap) -> Vec<String> {
        order
            .iter()
            .map(|(_, name)| String::from_utf8(name.as_ref().to_vec()).unwrap())
            .collect()
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn position(order: &[String], name: &str) -> usize {
        order
            .iter()
            .position(|listed| listed == name)
            .unwrap_or_else(|| panic!("{name} missing from {order:?}"))
    }

    #[test]
    fn header_order_without_extras_is_the_profile_order() {
        let order = HeaderOrder::new(None, &firefox_order(), Vec::new());
        assert_eq!(spelled(&order.with(&[])), spelled(&firefox_order()));
        assert!(!order.misses(&[]));
    }

    #[test]
    fn header_order_places_unknown_headers_before_content_type_in_the_order_given() {
        let order = HeaderOrder::new(None, &firefox_order(), strings(&["X-Client", "cookie"]));
        let names = spelled(&order.with(&strings(&["x-b", "X-A", "Referer", "x-client"])));

        let start = position(&names, "Range") + 1;
        assert_eq!(names[start..start + 4], ["X-Client", "x-b", "X-A", "Content-Type"]);
        assert_eq!(names.len(), spelled(&firefox_order()).len() + 3);
        assert_eq!(names.last().unwrap(), "TE");
    }

    #[test]
    fn header_order_needs_a_request_order_only_for_new_names() {
        let order = HeaderOrder::new(None, &firefox_order(), strings(&["X-Client"]));
        assert!(!order.misses(&strings(&["cookie", "REFERER", "x-client"])));
        assert!(order.misses(&strings(&["cookie", "x-new"])));
        assert!(!order.misses(&strings(&["not a header name"])));
    }

    #[test]
    fn header_order_puts_the_users_names_first_with_their_spelling() {
        let user = order_of(&["host", "ACCEPT"]);
        let order = HeaderOrder::new(Some(&user), &firefox_order(), Vec::new());
        let names = spelled(&order.with(&strings(&["X-A"])));

        assert_eq!(names[..4], ["host", "ACCEPT", "User-Agent", "Accept-Language"]);
        assert_eq!(names.iter().filter(|name| name.eq_ignore_ascii_case("accept")).count(), 1);
        assert!(position(&names, "X-A") + 1 == position(&names, "Content-Type"));
    }

    #[test]
    fn header_order_keeps_unknown_headers_ahead_of_the_rest_when_the_user_orders_content_type() {
        let user = order_of(&["Content-Type", "content-length"]);
        let order = HeaderOrder::new(Some(&user), &firefox_order(), Vec::new());
        let names = spelled(&order.with(&strings(&["X-A", "X-B"])));

        assert_eq!(names[..2], ["Content-Type", "content-length"]);
        let start = position(&names, "X-A");
        assert_eq!(names[start - 1..start + 3], ["Range", "X-A", "X-B", "Origin"]);
        assert_eq!(names.last().unwrap(), "TE");
    }

    #[test]
    fn http2_only_wins_over_http1_only() {
        assert!(restricted_to_http1(true, false));
        assert!(!restricted_to_http1(true, true));
        assert!(!restricted_to_http1(false, true));
        assert!(!restricted_to_http1(false, false));
    }
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    static READY: AtomicBool = AtomicBool::new(false);
    static LIVE: AtomicUsize = AtomicUsize::new(0);

    struct DropProbe;

    impl DropProbe {
        fn new() -> Self {
            LIVE.fetch_add(1, Ordering::SeqCst);
            Self
        }
    }

    impl Drop for DropProbe {
        fn drop(&mut self) {
            LIVE.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn probe_ready() -> bool {
        READY.load(Ordering::SeqCst)
    }

    fn drop_probe() -> Result<(), magnus::Error> {
        let _caller_resource = DropProbe::new();
        let result = unsafe {
            without_gvl(|token| {
                let _callback_resource = DropProbe::new();
                READY.store(true, Ordering::SeqCst);
                runtime().block_on(token.cancelled());
                DropProbe::new()
            })
        }?;
        drop(result);
        Ok(())
    }

    #[test]
    fn ruby_interrupts_drop_native_resources() {
        let ruby = unsafe { magnus::embed::init() };
        let result = unsafe { without_gvl(|_| DropProbe::new()) }.unwrap();
        assert_eq!(LIVE.load(Ordering::SeqCst), 1);
        drop(result);
        assert_eq!(LIVE.load(Ordering::SeqCst), 0);

        let panicked = panic::catch_unwind(|| unsafe {
            without_gvl::<_, ()>(|_| {
                let _resource = DropProbe::new();
                panic!("probe panic");
            })
        });
        assert!(panicked.is_err());
        assert_eq!(LIVE.load(Ordering::SeqCst), 0);

        ruby.define_global_function("wreq_drop_probe", function!(drop_probe, 0));
        ruby.define_global_function("wreq_probe_ready", function!(probe_ready, 0));

        for (interrupt, expected) in [
            ("worker.kill", None),
            ("worker.raise(RuntimeError, 'stop request')", Some("stop request")),
        ] {
            READY.store(false, Ordering::SeqCst);
            let result: Option<String> = ruby.eval(&format!(
                "worker = Thread.new do
                   begin
                     wreq_drop_probe
                     'returned normally'
                   rescue RuntimeError => error
                     error.message
                   end
                 end
                 Thread.pass until wreq_probe_ready
                 {interrupt}
                 worker.value"
            )).unwrap();

            assert_eq!(result.as_deref(), expected);
            assert_eq!(LIVE.load(Ordering::SeqCst), 0, "resources leaked after {interrupt}");
        }
    }
}
