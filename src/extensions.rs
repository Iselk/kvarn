//! Here, all extensions code is housed.
//!
//! Check out [extensions.md](https://github.com/Icelk/kvarn/tree/main/extensions.md) for more info.
//!
//! If you want to make new extensions for others to use, make sure to check other extensions,
//! so the priorities are valid. This can be done by using the debug implementation on [`Extensions`].
//! ```
//! # use kvarn::prelude::*;
//! let extensions = Extensions::new();
//! println!("The currently mounted extensions: {:#?}", extensions);
//! ```
//!
//! ## Unsafe pointers
//!
//! This modules contains extensive usage of unsafe pointers.
//!
//! ### Background
//!
//! In the extension code, I sometimes have to pass references of data to `Futures` to avoid cloning,
//! which sometimes is not an option (such as when a `TcpStream` is part of said data).
//! You cannot share references with `Futures`, and so I've opted to go the unsafe route. Literally.
//!
//! ### Safety
//!
//! In this module, there are several `Wrapper` types. They ***must not*** be stored.
//! It's safe to get the underlying type inside the extension which received the data;
//! the future is awaited and the referenced data is guaranteed to not be touched by
//! anyone but the receiving extension. If you use it later, the data can be used
//! or have been dropped.

use crate::prelude::{internals::*, *};

/// A return type for a `dyn` [`Future`].
///
/// Used as the return type for all extensions,
/// so they can be stored.
pub type RetFut<T> = Pin<Box<(dyn Future<Output = T> + Send)>>;
/// Same as [`RetFut`] but also implementing [`Sync`].
///
/// Mostly used for extensions used across yield bounds.
pub type RetSyncFut<T> = Pin<Box<dyn Future<Output = T> + Send + Sync>>;

/// A prime extension.
///
/// See [module level documentation](extensions) and the extensions.md link for more info.
pub type Prime =
    Box<(dyn Fn(RequestWrapper, HostWrapper, SocketAddr) -> RetFut<Option<Uri>> + Sync + Send)>;
/// A prepare extension.
///
/// See [module level documentation](extensions) and the extensions.md link for more info.
pub type Prepare = Box<
    (dyn Fn(RequestWrapperMut, HostWrapper, PathOptionWrapper, SocketAddr) -> RetFut<FatResponse>
         + Sync
         + Send),
>;
/// A present extension.
///
/// See [module level documentation](extensions) and the extensions.md link for more info.
pub type Present = Box<(dyn Fn(PresentDataWrapper) -> RetFut<()> + Sync + Send)>;
/// A package extension.
///
/// See [module level documentation](extensions) and the extensions.md link for more info.
pub type Package =
    Box<(dyn Fn(EmptyResponseWrapperMut, RequestWrapper, HostWrapper) -> RetFut<()> + Sync + Send)>;
/// A post extension.
///
/// See [module level documentation](extensions) and the extensions.md link for more info.
pub type Post = Box<
    (dyn Fn(RequestWrapper, HostWrapper, ResponsePipeWrapperMut, Bytes, SocketAddr) -> RetFut<()>
         + Sync
         + Send),
>;
/// Dynamic function to check if a extension should be ran.
///
/// Used with [`Prepare`] extensions
pub type If = Box<(dyn Fn(&FatRequest, &Host) -> bool + Sync + Send)>;
/// A [`Future`] for writing to a [`ResponsePipe`] after the response is sent.
///
/// Used with [`Prepare`] extensions
pub type ResponsePipeFuture = Box<
    dyn FnOnce(extensions::ResponseBodyPipeWrapperMut, extensions::HostWrapper) -> RetSyncFut<()>
        + Send
        + Sync,
>;

/// A extension Id. The [`Self::priority`] is used for sorting extensions
/// and [`Self::name`] for debugging which extensions are mounted.
///
/// Higher `priority` extensions are ran first.
/// The debug name is useful when you want to see which priorities
/// other extensions use. This is beneficial when creating "plug-and-play" extensions.
#[derive(Debug, Clone, Copy)]
#[must_use]
pub struct Id {
    priority: i32,
    name: Option<&'static str>,
    no_override: bool,
}
impl Id {
    /// Creates a new Id with `priority` and a `name`.
    pub fn new(priority: i32, name: &'static str) -> Self {
        Self {
            priority,
            name: Some(name),
            no_override: false,
        }
    }
    /// Creates a Id without a name. This is considered a bad practice,
    /// as you cannot see which extensions are mounted to the
    /// [`Extensions`].
    ///
    /// See [`Self::name`] for details about how this affects output.
    pub fn without_name(priority: i32) -> Self {
        Self {
            priority,
            name: None,
            no_override: false,
        }
    }
    /// Always inserts this extension.
    /// If an extensions with the same Id exist, the Id is decremented and tried again.
    pub fn no_override(mut self) -> Self {
        self.no_override = true;
        self
    }
    /// Returns the name of this Id.
    ///
    /// If the Id is created with [`Self::without_name`],
    /// this returns `Unnamed`.
    #[must_use]
    pub fn name(&self) -> &'static str {
        self.name.unwrap_or("Unnamed")
    }
    /// Returns the priority of this extension.
    #[must_use]
    pub fn priority(&self) -> i32 {
        self.priority
    }
}
impl Display for Id {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "\"{}\" with priority {}", self.name(), self.priority())
    }
}
impl PartialEq for Id {
    fn eq(&self, other: &Self) -> bool {
        self.priority().eq(&other.priority())
    }
}
impl Eq for Id {}
impl Ord for Id {
    fn cmp(&self, other: &Self) -> cmp::Ordering {
        self.priority().cmp(&other.priority())
    }
}
impl PartialOrd for Id {
    fn partial_cmp(&self, other: &Self) -> Option<cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Returns a future accepted by all the [`extensions`]
/// yielding immediately with `value`.
#[inline]
pub fn ready<T: 'static + Send>(value: T) -> RetFut<T> {
    Box::pin(core::future::ready(value))
}

macro_rules! add_sort_list {
    ($list: expr, $id: expr, $($other: expr, )+) => {
        let mut id = $id;
        loop {
            match $list.binary_search_by(|probe| id.cmp(&probe.0)) {
                Ok(_) if id.no_override => {
                    if let Some(priority) = id.priority.checked_sub(1) {
                        id.priority = priority;
                    } else {
                        panic!("reached minimum priority when trying to not override extension");
                    }
                    continue;
                }
                Ok(pos) => {
                    $list[pos] = (id, $($other, )*);
                    break;
                }
                Err(pos) => {
                    $list.insert(pos, (id, $($other, )*));
                    break;
                }
            };
        }
    };
}

/// Contains all extensions.
/// See [extensions.md](../extensions.md) for more info.
///
/// `ToDo`: remove and list? Give mut access to underlying `Vec`s and `HashMap`s or a `Entry`-like interface?
#[allow(missing_debug_implementations)]
#[must_use]
pub struct Extensions {
    prime: Vec<(Id, Prime)>,
    prepare_single: HashMap<String, Prepare>,
    prepare_fn: Vec<(Id, If, Prepare)>,
    present_internal: HashMap<String, Present>,
    present_file: HashMap<String, Present>,
    package: Vec<(Id, Package)>,
    post: Vec<(Id, Post)>,
}
impl Extensions {
    /// Creates a empty [`Extensions`].
    ///
    /// It is strongly recommended to use [`Extensions::new()`] instead.
    #[inline]
    pub fn empty() -> Self {
        Self {
            prime: Vec::new(),
            prepare_single: HashMap::new(),
            prepare_fn: Vec::new(),
            present_internal: HashMap::new(),
            present_file: HashMap::new(),
            package: Vec::new(),
            post: Vec::new(),
        }
    }
    /// Creates a new [`Extensions`] and adds a few essential extensions.
    ///
    /// For now the following extensions are added. The number in parentheses are the priority.
    /// - a Prime extension (-64) redirecting the user from `<path>/` to `<path>/index.html` and
    ///   `<path>.` to `<path>.html` is included.
    ///   This was earlier part of parsing of the path, but was moved to an extension for consistency and performance; now `/`, `index.`, and `index.html` is the same entity in cache.
    /// - Package extension (8) to set `Referrer-Policy` header to `no-referrer` for max security and privacy.
    ///   This is only done when no other `Referrer-Policy` header has been set earlier in the response.
    /// - A CORS extension to deny all CORS requests. See [`Self::add_cors`] for CORS management.
    pub fn new() -> Self {
        let mut new = Self::empty();

        new.add_uri_redirect().add_no_referrer().add_disallow_cors();

        new
    }

    /// Adds a prime extension to redirect [`Uri`]s ending with `.` and `/`.
    ///
    /// This routs the requests according to [`host::Options::folder_default`] and
    /// [`host::Options::extension_default`].
    /// See respective documentation for more info.
    pub fn add_uri_redirect(&mut self) -> &mut Self {
        self.add_prime(
            Box::new(|request, host, _| {
                enum Ending {
                    Dot,
                    Slash,
                    Other,
                }
                impl Ending {
                    fn from_uri(uri: &Uri) -> Self {
                        if uri.path().ends_with('.') {
                            Self::Dot
                        } else if uri.path().ends_with('/') {
                            Self::Slash
                        } else {
                            Self::Other
                        }
                    }
                }
                let uri: &Uri = unsafe { request.get_inner() }.uri();
                let host: &Host = unsafe { host.get_inner() };
                let append = match Ending::from_uri(uri) {
                    Ending::Other => return ready(None),
                    Ending::Dot => host.options.extension_default.as_deref().unwrap_or("html"),
                    Ending::Slash => host
                        .options
                        .folder_default
                        .as_deref()
                        .unwrap_or("index.html"),
                };

                let mut uri = uri.clone().into_parts();

                let path = uri
                    .path_and_query
                    .as_ref()
                    .map_or("/", uri::PathAndQuery::path);
                let query = uri
                    .path_and_query
                    .as_ref()
                    .and_then(uri::PathAndQuery::query);
                let path_and_query = build_bytes!(
                    path.as_bytes(),
                    append.as_bytes(),
                    if query.is_none() { "" } else { "?" }.as_bytes(),
                    query.unwrap_or("").as_bytes()
                );

                // This is ok, we only added bytes from a String, which are guaranteed to be valid for a URI path
                uri.path_and_query =
                    Some(uri::PathAndQuery::from_maybe_shared(path_and_query).unwrap());

                // Again ok, see ↑
                let uri = Uri::from_parts(uri).unwrap();

                ready(Some(uri))
            }),
            Id::new(-100, "Expanding . and / to reduce URI size"),
        );
        self
    }
    /// Adds a [`Package`] extension to set the `referrer-policy` to `no-referrer`
    /// for maximum privacy and security.
    /// This is added when calling [`Extensions::new`].
    pub fn add_no_referrer(&mut self) -> &mut Self {
        self.add_package(
            Box::new(|mut response, _, _| {
                let response: &mut Response<()> = unsafe { response.get_inner() };
                response
                    .headers_mut()
                    .entry("referrer-policy")
                    .or_insert(HeaderValue::from_static("no-referrer"));

                ready(())
            }),
            Id::new(10, "Set the referrer-policy header to no-referrer"),
        );
        self
    }
    /// Adds extensions to disallow all CORS requests.
    /// This is added when calling [`Extensions::new`].
    pub fn add_disallow_cors(&mut self) -> &mut Self {
        self.add_prime(
            Box::new(|request, _, _| {
                box_fut!({
                    let request = unsafe { request.get_inner() };

                    let missmatch = request
                        .headers()
                        .get("origin")
                        .and_then(|origin| origin.to_str().ok())
                        .map_or(false, |origin| {
                            !Cors::is_part_of_origin(origin, request.uri())
                        });
                    if missmatch {
                        Some(Uri::from_static("/./cors_fail"))
                    } else {
                        None
                    }
                })
            }),
            Id::new(16_777_216, "Reroute all CORS requests to /./cors_fail"),
        );

        self.add_prepare_single(
            "/./cors_fail".to_owned(),
            Box::new(|_, _, _, _| {
                box_fut!({
                    let response = Response::builder()
                        .status(StatusCode::FORBIDDEN)
                        .body(Bytes::from_static(b"CORS request denied"))
                        .expect("we know this is a good request.");
                    FatResponse::new(response, ServerCachePreference::Full)
                })
            }),
        );
        self
    }
    /// Overrides the default handling (deny all) of CORS requests to be `cors_settings`.
    ///
    /// See [`Cors`] for an example and more info.
    pub fn add_cors(&mut self, cors_settings: Arc<Cors>) -> &mut Self {
        self.add_disallow_cors();

        let options_cors_settings = Arc::clone(&cors_settings);
        let package_cors_settings = Arc::clone(&cors_settings);

        // This priority have to be higher than the one in the [`Self::add_disallow_cors`]'s prime
        // extension.
        self.add_prime(
            Box::new(move |request, _, _| {
                let request = unsafe { request.get_inner() };

                let allow = cors_settings.check_cors_request(request);
                ready(if allow.is_some() {
                    None
                } else {
                    Some(Uri::from_static("/./cors_fail"))
                })
            }),
            Id::new(
                16_777_216,
                "Reroute not allowed CORS request to /./cors_failed",
            ),
        );

        // Low priority so it runs last.
        self.add_package(
            Box::new(move |mut response, request, _| {
                let (response, request) = unsafe { (response.get_inner(), request.get_inner()) };

                if let Some(origin) = request.headers().get("origin") {
                    let allowed = package_cors_settings.check_cors_request(request).is_some();
                    if allowed {
                        utils::replace_header(
                            response.headers_mut(),
                            "access-control-allow-origin",
                            origin.clone(),
                        )
                    }
                }
                ready(())
            }),
            Id::new(
                -1024,
                "Adds access-control-allow-origin depending on if CORS request is allowed",
            ),
        );

        self.add_prepare_single(
            "/./cors_options".to_owned(),
            Box::new(move |mut request, _, _, _| {
                let request = unsafe { request.get_inner() };
                let allowed = options_cors_settings.check_cors_request(request);

                let mut builder = Response::builder().status(StatusCode::NO_CONTENT);

                if let Some((methods, headers)) = allowed {
                    let methods = methods
                        .iter()
                        .enumerate()
                        .fold(BytesMut::with_capacity(24), |mut acc, (pos, method)| {
                            acc.extend_from_slice(method.as_str().as_bytes());
                            if pos + 1 != methods.len() {
                                acc.extend_from_slice(b", ");
                            }
                            acc
                        })
                        .freeze();
                    let headers = headers
                        .iter()
                        .enumerate()
                        .fold(BytesMut::with_capacity(24), |mut acc, (pos, header)| {
                            acc.extend_from_slice(header.as_str().as_bytes());
                            if pos + 1 != headers.len() {
                                acc.extend_from_slice(b", ");
                            }
                            acc
                        })
                        .freeze();

                    builder = builder
                        .header(
                            "access-control-allow-methods",
                            HeaderValue::from_maybe_shared(methods).unwrap(),
                        )
                        .header(
                            "access-control-allow-headers",
                            HeaderValue::from_maybe_shared(headers).unwrap(),
                        );
                }

                let response = builder.body(Bytes::new()).unwrap_or_else(|_| {
                    Response::builder()
                        .status(StatusCode::INTERNAL_SERVER_ERROR)
                        .body(utils::hardcoded_error_body(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            None,
                        ))
                        .expect("this is a good response.")
                });
                ready(FatResponse::new(response, ServerCachePreference::None))
            }),
        );

        // This priority has to be above all the above, else it won't be able to get the options.
        self.add_prime(
            Box::new(move |request, _, _| {
                let request = unsafe { request.get_inner() };
                ready(
                    if request.method() == Method::OPTIONS
                        && request.headers().get("origin").is_some()
                        && request
                            .headers()
                            .get("access-control-request-method")
                            .is_some()
                    {
                        Some(Uri::from_static("/./cors_options"))
                    } else {
                        None
                    },
                )
            }),
            Id::new(16_777_215, "Provides CORS preflight request support"),
        );

        self
    }

    /// Adds a prime extension. Higher [`Id::priority()`] extensions are ran first.
    pub fn add_prime(&mut self, extension: Prime, id: Id) {
        add_sort_list!(self.prime, id, extension,);
    }
    /// Adds a prepare extension for a single URI.
    pub fn add_prepare_single(&mut self, path: String, extension: Prepare) {
        self.prepare_single.insert(path, extension);
    }
    /// Adds a prepare extension run if `function` return `true`. Higher [`Id::priority()`] extensions are ran first.
    pub fn add_prepare_fn(&mut self, predicate: If, extension: Prepare, id: Id) {
        add_sort_list!(self.prepare_fn, id, predicate, extension,);
    }
    /// Adds a present internal extension, called with files starting with `!> `.
    pub fn add_present_internal(&mut self, name: String, extension: Present) {
        self.present_internal.insert(name, extension);
    }
    /// Adds a present file extension, called with file extensions matching `name`.
    pub fn add_present_file(&mut self, name: String, extension: Present) {
        self.present_file.insert(name, extension);
    }
    /// Adds a package extension, used to make last-minute changes to response. Higher [`Id::priority()`] extensions are ran first.
    pub fn add_package(&mut self, extension: Package, id: Id) {
        add_sort_list!(self.package, id, extension,);
    }
    /// Adds a post extension, used for HTTP/2 push Higher [`Id::priority()`] extensions are ran first.
    pub fn add_post(&mut self, extension: Post, id: Id) {
        add_sort_list!(self.post, id, extension,);
    }

    pub(crate) async fn resolve_prime(
        &self,
        request: &mut FatRequest,
        host: &Host,
        address: SocketAddr,
    ) -> Option<Uri> {
        let mut uri = None;
        for (_, prime) in &self.prime {
            if let Some(prime) = prime(
                RequestWrapper::new(request),
                HostWrapper::new(host),
                address,
            )
            .await
            {
                if prime.path().starts_with("/./") {
                    uri = Some(prime)
                } else {
                    *request.uri_mut() = prime;
                }
            }
        }
        uri
    }
    pub(crate) async fn resolve_prepare(
        &self,
        request: &mut FatRequest,
        overide_uri: Option<&Uri>,
        host: &Host,
        path: &Option<PathBuf>,
        address: SocketAddr,
    ) -> Option<FatResponse> {
        if let Some(extension) = self
            .prepare_single
            .get(overide_uri.unwrap_or_else(|| request.uri()).path())
        {
            Some(
                extension(
                    RequestWrapperMut::new(request),
                    HostWrapper::new(host),
                    PathOptionWrapper::new(path),
                    address,
                )
                .await,
            )
        } else {
            for (_, function, extension) in &self.prepare_fn {
                if function(request, host) {
                    return Some(
                        extension(
                            RequestWrapperMut::new(request),
                            HostWrapper::new(host),
                            PathOptionWrapper::new(path),
                            address,
                        )
                        .await,
                    );
                }
            }
            None
        }
    }
    // It's an internal function, which should be the same style as all the other `resolve_*` functions.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn resolve_present(
        &self,
        request: &mut Request<Body>,
        response: &mut Response<Bytes>,
        client_cache_preference: &mut ClientCachePreference,
        server_cache_preference: &mut ServerCachePreference,
        host: &Host,
        address: SocketAddr,
    ) -> io::Result<()> {
        let mut body = LazyRequestBody::new(request.body_mut());
        let body = &mut body;
        let path = utils::parse::uri(request.uri().path());

        if let Some(extensions) = PresentExtensions::new(Bytes::clone(response.body())) {
            *response.body_mut() = response.body_mut().split_off(extensions.data_start());
            for extension_name_args in extensions {
                if let Some(extension) = self.present_internal.get(extension_name_args.name()) {
                    let mut data = PresentData {
                        address,
                        request,
                        body,
                        host,
                        path: path.map(|p| p as *const _),
                        server_cache_preference,
                        client_cache_preference,
                        response,
                        args: extension_name_args,
                    };
                    let data = PresentDataWrapper::new(&mut data);
                    extension(data).await;
                }
            }
        }
        if let Some(extension) = path
            .and_then(Path::extension)
            .and_then(std::ffi::OsStr::to_str)
            .and_then(|s| self.present_file.get(s))
        {
            let mut data = PresentData {
                address,
                request,
                body,
                host,
                path: path.map(|p| p as *const _),
                server_cache_preference,
                client_cache_preference,
                response,
                args: PresentArguments::empty(),
            };
            let data = PresentDataWrapper::new(&mut data);
            extension(data).await;
        }
        Ok(())
    }
    pub(crate) async fn resolve_package(
        &self,
        response: &mut Response<()>,
        request: &FatRequest,
        host: &Host,
    ) {
        for (_, extension) in &self.package {
            extension(
                EmptyResponseWrapperMut::new(response),
                RequestWrapper::new(request),
                HostWrapper::new(host),
            )
            .await;
        }
    }
    pub(crate) async fn resolve_post(
        &self,
        request: &FatRequest,
        bytes: Bytes,
        response_pipe: &mut ResponsePipe,
        addr: SocketAddr,
        host: &Host,
    ) {
        for (_, extension) in self.post.iter().take(self.post.len().saturating_sub(1)) {
            extension(
                RequestWrapper::new(request),
                HostWrapper::new(host),
                ResponsePipeWrapperMut::new(response_pipe),
                Bytes::clone(&bytes),
                addr,
            )
            .await;
        }
        if let Some((_, extension)) = self.post.last() {
            extension(
                RequestWrapper::new(request),
                HostWrapper::new(host),
                ResponsePipeWrapperMut::new(response_pipe),
                bytes,
                addr,
            )
            .await;
        }
    }
}
impl Default for Extensions {
    fn default() -> Self {
        Self::new()
    }
}
impl Debug for Extensions {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        macro_rules! map {
            ($slice: expr) => {
                &$slice
                    .iter()
                    .map(|ext| (ext.0.as_clean(), "internal extension".as_clean()))
                    .collect::<Vec<_>>()
            };
        }
        f.debug_struct("Extensions")
            .field("prime", map!(self.prime))
            .field("prepare_single", map!(self.prepare_single))
            .field("prepare_fn", map!(self.prepare_fn))
            .field("present_internal", map!(self.present_internal))
            .field("present_file", map!(self.present_file))
            .field("package", map!(self.package))
            .field("post", map!(self.post))
            .finish()
    }
}

macro_rules! get_unsafe_wrapper {
    ($main:ident, $return:ty, $ret_str:expr) => {
        #[doc = "A wrapper type for `"]
        #[doc = $ret_str]
        #[doc = "`.\n\nSee [module level documentation](crate::extensions) for more information."]
        #[allow(missing_debug_implementations)]
        #[must_use]
        pub struct $main(*const $return);
        impl $main {
            pub(crate) fn new(data: &$return) -> Self {
                Self(data)
            }
            /// # Safety
            ///
            /// See [module level documentation](crate::extensions).
            #[inline]
            #[must_use = "must use extracted reference"]
            pub unsafe fn get_inner(&self) -> &$return {
                &*self.0
            }
        }
        unsafe impl Send for $main {}
        unsafe impl Sync for $main {}
    };
    ($main:ident, $return:ty) => {
        get_unsafe_wrapper!($main, $return, stringify!($return));
    };
}
macro_rules! get_unsafe_mut_wrapper {
    ($main:ident, $return:ty, $ret_str:expr) => {
        #[doc = "A wrapper type for `"]
        #[doc = $ret_str]
        #[doc = "`.\n\nSee [module level documentation](crate::extensions) for more information."]
        #[allow(missing_debug_implementations)]
        #[must_use]
        pub struct $main(*mut $return);
        impl $main {
            pub(crate) fn new(data: &mut $return) -> Self {
                Self(data)
            }
            /// # Safety
            ///
            /// See [module level documentation](crate::extensions).
            #[inline]
            #[must_use = "must use extracted reference"]
            pub unsafe fn get_inner(&mut self) -> &mut $return {
                &mut *self.0
            }
        }
        unsafe impl Send for $main {}
        unsafe impl Sync for $main {}
    };
    ($main:ident, $return:ty) => {
        get_unsafe_mut_wrapper!($main, $return, stringify!($return));
    };
}
/// **Should only be used from within Kvarn!**
/// A super-unsafe pointer. Must be used with great care.
///
/// # Safety
///
/// The safety of this struct requires you to
/// 1. make sure the lifetime of the `reference` lives longer than all references returned by
///    [`Self::get`] and [`Self::get_mut`].
/// 2. make sure no other references exist.
///    You can use [`Self::new_mut`] to garantuee this.
///
/// This type implements `Send + Sync` to allow it to cross `async` boundarys.
/// This type is used in the `move ||` part of extensions.
#[repr(transparent)]
#[must_use]
#[allow(missing_debug_implementations)]
pub struct SuperUnsafePointer<T> {
    pointer: *mut T,
}
impl<T> SuperUnsafePointer<T> {
    /// Creates a new pointer from a mutable reference.
    ///
    /// # Safety
    ///
    /// Read the docs of this struct for safety requirements.
    ///
    /// The compiler guarantees exclusive mutable access.
    pub fn new_mut(reference: &mut T) -> Self {
        Self { pointer: reference }
    }
    /// Creates a new pointer from a reference.
    ///
    /// # Safety
    ///
    /// Read the docs of this struct for safety requirements.
    pub unsafe fn new(reference: &T) -> Self {
        Self {
            pointer: reference as *const _ as *mut _,
        }
    }
    /// Get a reference to the inner value.
    ///
    /// # Safety
    ///
    /// Read the docs of this struct for safety requirements.
    #[must_use]
    pub unsafe fn get(&self) -> &T {
        &*self.pointer
    }
    /// Get a mutable reference to the inner value.
    ///
    /// # Safety
    ///
    /// Read the docs of this struct for safety requirements.
    pub unsafe fn get_mut(&mut self) -> &mut T {
        &mut *self.pointer
    }
}
unsafe impl<T: Send> Send for SuperUnsafePointer<T> {}
unsafe impl<T: Sync> Sync for SuperUnsafePointer<T> {}

get_unsafe_wrapper!(RequestWrapper, FatRequest);
get_unsafe_mut_wrapper!(RequestWrapperMut, FatRequest);
get_unsafe_mut_wrapper!(EmptyResponseWrapperMut, Response<()>);
get_unsafe_mut_wrapper!(ResponsePipeWrapperMut, ResponsePipe);
get_unsafe_wrapper!(HostWrapper, Host);
get_unsafe_wrapper!(PathOptionWrapper, Option<PathBuf>);
get_unsafe_mut_wrapper!(PresentDataWrapper, PresentData);
get_unsafe_mut_wrapper!(ResponseBodyPipeWrapperMut, ResponseBodyPipe);

/// Add data pretending to present state in creating the response.
///
/// Can be acquired from [`PresentDataWrapper`].
///
/// See [module level documentation](crate::extensions).
#[allow(missing_debug_implementations)]
pub struct PresentData {
    // Regarding request
    address: SocketAddr,
    request: *const FatRequest,
    body: *mut LazyRequestBody,
    host: *const Host,
    path: Option<*const Path>,
    // Regarding response
    server_cache_preference: *mut ServerCachePreference,
    client_cache_preference: *mut ClientCachePreference,
    response: *mut Response<Bytes>,
    // Regarding extension
    args: PresentArguments,
}
#[allow(missing_docs)]
impl PresentData {
    #[inline]
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    #[inline]
    pub fn request(&self) -> &FatRequest {
        unsafe { &*self.request }
    }
    #[inline]
    pub fn body(&mut self) -> &mut LazyRequestBody {
        unsafe { &mut *self.body }
    }
    #[inline]
    pub fn host(&self) -> &Host {
        unsafe { &*self.host }
    }
    #[inline]
    pub fn path(&self) -> Option<&Path> {
        unsafe { self.path.map(|p| &*p) }
    }
    #[inline]
    pub fn server_cache_preference(&mut self) -> &mut ServerCachePreference {
        unsafe { &mut *self.server_cache_preference }
    }
    #[inline]
    pub fn client_cache_preference(&mut self) -> &mut ClientCachePreference {
        unsafe { &mut *self.client_cache_preference }
    }
    #[inline]
    pub fn response_mut(&mut self) -> &mut Response<Bytes> {
        unsafe { &mut *self.response }
    }
    #[inline]
    pub fn response(&self) -> &Response<Bytes> {
        unsafe { &*self.response }
    }
    #[inline]
    pub fn args(&self) -> &PresentArguments {
        &self.args
    }
}
unsafe impl Send for PresentData {}
unsafe impl Sync for PresentData {}

/// A [`Request`] [`Body`] which is lazily read.
#[derive(Debug)]
#[must_use]
pub struct LazyRequestBody {
    body: *mut Body,
    result: Option<Bytes>,
}
impl LazyRequestBody {
    /// This struct must be `dropped` before `body` or Undefined Behaviour occurs.
    ///
    /// The `body` is converted to a `*mut` which can be dereferenced safely, as long as we wait for this to be dropped.
    /// It can also not be referenced in any other way while this is not dropped.
    #[inline]
    pub(crate) fn new(body: &mut Body) -> Self {
        Self { body, result: None }
    }
    /// Reads the `Bytes` from the request body.
    ///
    /// # Errors
    ///
    /// Returns any errors from reading the inner [`Body`].
    #[inline]
    pub async fn get(&mut self) -> io::Result<&Bytes> {
        if let Some(ref result) = self.result {
            Ok(result)
        } else {
            let buffer = unsafe { &mut *self.body }.read_to_bytes().await?;
            self.result.replace(buffer);
            // ok; we've just assigned to it
            Ok(self.result.as_ref().unwrap())
        }
    }
}
unsafe impl Send for LazyRequestBody {}
unsafe impl Sync for LazyRequestBody {}

/// A [CORS](https://en.wikipedia.org/wiki/Cross-origin_resource_sharing) ruleset for Kvarn.
///
/// Use [`Extensions::add_cors`] to allow selected CORS requests.
///
/// # Examples
///
/// ```
/// # use kvarn::prelude::*;
/// // Allow `https://icelk.dev` and `https://kvarn.org` to access all images.
/// // Also allow all requests from `http://example.org` access to the api.
/// let cors =
///     Cors::new()
///         .allow(
///             "/images/*",
///             CorsAllowList::new()
///                 .add_origin("https://icelk.dev")
///                 .add_origin("https://kvarn.org")
///             )
///         .allow(
///             "/api/*",
///             CorsAllowList::new()
///                 .add_origin("http://example.org")
///                 .add_method(Method::PUT)
///                 .add_method(Method::POST)
///         );
/// ```
#[must_use]
#[derive(Debug)]
pub struct Cors {
    rules: Vec<(String, CorsAllowList)>,
}
impl Cors {
    /// Creates a new CORS ruleset without any rules.
    /// All CORS requests are rejected.
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }
    /// Allows requests from the `allow_list` to access `path`.
    ///
    /// By default, `path` will only match requests with the exact path.
    /// This can be changed by appending `*` to the end of the path, which
    /// will then check if the request path start with `path`.
    pub fn allow(mut self, path: impl AsRef<str>, allow_list: CorsAllowList) -> Self {
        let path = path.as_ref().to_owned();

        self.rules.push((path, allow_list));

        self.rules.sort_by(|a, b| {
            use std::cmp::Ordering;
            if a.0.ends_with('*') == b.0.ends_with('*') {
                b.0.len().cmp(&a.0.len())
            } else if a.0.ends_with('*') {
                Ordering::Greater
            } else {
                Ordering::Less
            }
        });

        self
    }
    /// Puts `self` in a [`Arc`].
    /// Useful for adding the CORS ruleset with [`Extensions::add_cors`].
    #[must_use]
    pub fn build(self) -> Arc<Self> {
        Arc::new(self)
    }
    /// Check if the (cross-origin) request's `origin` [`Uri`] is allowed by the CORS rules.
    pub fn check_origin(&self, origin: &Uri, uri_path: &str) -> Option<(&[Method], &[HeaderName])> {
        for (path, allow) in &self.rules {
            if path == uri_path
                || (path
                    .strip_suffix('*')
                    .map_or(false, |path| uri_path.starts_with(path)))
            {
                return allow.check(origin);
            }
        }
        None
    }
    /// Check if the `headers` and `uri` is allowed with this ruleset.
    ///
    /// > This will not check for errors in `access-control-request-headers`.
    ///
    /// Use this over [`Self::check_origin`] becuse this checks for `same_origin` requests.
    pub fn check_cors_request<T>(
        &self,
        request: &Request<T>,
    ) -> Option<(&[Method], &[HeaderName])> {
        let same_origin_allowed_headers =
            (&[Method::GET, Method::HEAD, Method::OPTIONS][..], &[][..]);
        match request.headers().get("origin") {
            None => Some(same_origin_allowed_headers),
            Some(origin)
                if origin.to_str().map_or(false, |origin| {
                    Cors::is_part_of_origin(origin, request.uri())
                }) =>
            {
                Some(same_origin_allowed_headers)
            }
            Some(origin) => match Uri::try_from(origin.as_bytes()) {
                Ok(origin) => match self.check_origin(&origin, request.uri().path()) {
                    Some(origin) if origin.0.contains(request.method()) => Some(origin),
                    _ => None,
                },
                Err(_) => None,
            },
        }
    }
    /// Checks if `uri` is the same origin as `origin`.
    fn is_part_of_origin(origin: &str, uri: &Uri) -> bool {
        let origin = match origin.strip_prefix("https://") {
            Some(o) if uri.scheme() == Some(&uri::Scheme::HTTPS) => o,
            None => match origin.strip_prefix("http://") {
                Some(o) if uri.scheme() == Some(&uri::Scheme::HTTP) => o,
                _ => return false,
            },
            Some(_) => return false,
        };
        uri.authority()
            .map(uri::Authority::as_str)
            .map_or(false, |authority| authority == origin)
    }
}
impl Default for Cors {
    fn default() -> Self {
        Self::new()
    }
}
/// A CORS allow list which allowes hosts, methods, and headers from a associated path.
/// This is a builder-like struct.
/// Use the `add_*` methods to add allowed origins, methods, and headers.
/// Multiple allow lists can be added to a [`Cors`] instance.
/// See the example at [`Cors`].
///
/// Use [`Cors::allow`] to add a rule.
#[must_use]
#[derive(Debug)]
pub struct CorsAllowList {
    allowed: Vec<Uri>,
    allow_all_origins: bool,
    methods: Vec<Method>,
    headers: Vec<HeaderName>,
}
impl CorsAllowList {
    /// Creates a empty CORS allow list.
    pub fn new() -> Self {
        Self {
            allowed: Vec::new(),
            allow_all_origins: false,
            methods: vec![Method::GET, Method::HEAD, Method::OPTIONS],
            headers: Vec::new(),
        }
    }
    /// Allows CORS request from `allowed_origin`.
    /// Note that the scheme (`https` / `http`) is sensitive.
    /// Use [`Self::add_origin_uri`] for a [`Uri`] input.
    pub fn add_origin(self, allowed_origin: &'static str) -> Self {
        self.add_origin_uri(Uri::from_static(allowed_origin))
    }
    /// Allows CORS request from `allowed_origin`.
    /// Note that the scheme (`https` / `http`) is sensitive.
    pub fn add_origin_uri(mut self, allowed_origin: Uri) -> Self {
        assert!(allowed_origin.host().is_some());
        assert!(allowed_origin.scheme().is_some());
        self.allowed.push(allowed_origin);
        self
    }
    /// Enables the flag to allow all origins to use the set methods and headers in CORS requests.
    pub fn allow_all_origins(mut self) -> Self {
        self.allow_all_origins = true;
        self
    }
    /// Allows the listed origin(s) (added via [`Self::add_origin`])
    /// to request using `allowed_method`.
    pub fn add_method(mut self, allowed_method: Method) -> Self {
        if !self.methods.contains(&allowed_method) {
            self.methods.push(allowed_method)
        }
        self
    }
    /// Allows the listed origin(s) (added via [`Self::add_origin`])
    /// to send the `allowed_header` in the request.
    pub fn add_header(mut self, allowed_header: HeaderName) -> Self {
        if !self.headers.contains(&allowed_header) {
            self.headers.push(allowed_header)
        }
        self
    }
    /// Checks if the `origin` is allowed according to the allow list.
    pub fn check(&self, origin: &Uri) -> Option<(&[Method], &[HeaderName])> {
        if self.allow_all_origins {
            return Some((&self.methods, &self.headers));
        }
        for allowed in &self.allowed {
            let scheme = allowed.scheme().map_or("https", |scheme| scheme.as_str());
            if Some(allowed.host().unwrap()) == origin.host()
                && allowed.port_u16() == origin.port_u16()
                && Some(scheme) == origin.scheme().map(uri::Scheme::as_str)
            {
                return Some((&self.methods, &self.headers));
            }
        }
        None
    }
}
impl Default for CorsAllowList {
    fn default() -> Self {
        Self::new()
    }
}

mod macros {
    /// Makes a pinned future, compatible with [`crate::RetFut`] and [`crate::RetSyncFut`]
    ///
    /// # Examples
    ///
    /// This creates a future which prints `Hello world!` and awaits it.
    /// ```
    /// # async {
    /// # use kvarn::box_fut;
    /// let fut = box_fut!({
    ///     println!("Hello world!");
    /// });
    /// fut.await;
    /// # };
    /// ```
    #[macro_export]
    macro_rules! box_fut {
        ($code:block) => {
            Box::pin(async move { $code })
        };
    }

    /// The ultimate extension-creation macro.
    ///
    /// This is used in the various other macros which expand to extensions; **use them instead**!
    ///
    /// # Examples
    ///
    /// This is similar to the `prepare!` macro.
    /// ```
    /// # use kvarn::prelude::*;
    /// extension!(|
    ///     request: RequestWrapperMut,
    ///     host: HostWrapper,
    ///     path: PathOptionWrapper |
    ///     addr: SocketAddr |,
    ///     ,
    ///     { println!("Hello world, from extension macro!"); }
    /// );
    /// ```
    #[macro_export]
    macro_rules! extension {
        (| $($wrapper_param:ident: $wrapper_param_type:ty $(,)?)* |$(,)? $($param:ident: $param_type:ty $(,)?)* |, $($clone:ident)*, $code:block) => {{
            use $crate::extensions::*;
            #[allow(unused_mut)]
            Box::new(move |
                $(mut $wrapper_param: $wrapper_param_type,)*
                $(mut $param: $param_type,)*
            | {
                // SAFETY: This is safe because we know the future will be ran immediately when it's
                // returned.
                // The closure owns a Arc and Kvarn's internals guarantees the future will be
                // ran in the closure's lifetime.
                $(let $clone = unsafe { SuperUnsafePointer::new(&$clone) };)*
                Box::pin(async move {
                    // SAFETY: as stated in [`kvarn::extensions`], it's safe to get the inner
                    // value of wrapper struct inside the extension.
                    $(let $wrapper_param = unsafe { $wrapper_param.get_inner() };)*
                    // SAFETY: See the comments above.
                    $(let $clone = unsafe { $clone.get() };)*

                    $code
                }) as RetSyncFut<_>
            })
        }}
    }

    /// Will make a prime extension.
    ///
    /// See [`prepare!`] for usage and useful examples.
    ///
    /// # Examples
    /// ```
    /// # use kvarn::prelude::*;
    /// let extension = prime!(req, host, addr {
    ///     default_error_response(StatusCode::BAD_REQUEST, host, None).await
    /// });
    /// ```
    #[macro_export]
    macro_rules! prime {
        ($request:ident, $host:ident, $addr:ident $(, move |$($clone:ident $(,)?)+|)? $code:block) => {
            extension!(|$request: RequestWrapper, $host: HostWrapper | $addr: SocketAddr|, $($($clone)*)*, $code)
        }
    }
    /// Will make a prepare extension.
    ///
    /// See example bellow. Where `times_called` is defined in the arguments of the macro, you can enter several `Arc`s to capture from the environment.
    /// They will be cloned before being moved to the future, mitigating the error `cannot move out of 'times_called', a captured variable in an 'Fn' closure`.
    /// **Only `Arc`s** will work, since the variable has to be `Send` and `Sync`.
    ///
    /// You have to have kvarn imported as `kvarn`.
    ///
    /// # Examples
    ///
    /// > **These examples are applicable to all other extension-creation macros,
    /// > but with different parameters. See their respective documentation.**
    ///
    /// ```
    /// # use kvarn::prelude::*;
    /// use std::sync::{Arc, atomic};
    ///
    /// let times_called = Arc::new(atomic::AtomicUsize::new(0));
    ///
    /// prepare!(req, host, path, addr, move |times_called| {
    ///     let times_called = times_called.fetch_add(1, atomic::Ordering::Relaxed);
    ///     println!("Called {} time(s). Request {:?}", times_called, req);
    ///
    ///     default_error_response(StatusCode::NOT_FOUND, host, None).await
    /// });
    /// ```
    ///
    /// To capture no variables, just leave out the `move ||`.
    /// ```
    /// # use kvarn::prelude::*;
    /// prepare!(req, host, path, addr {
    ///     default_error_response(StatusCode::METHOD_NOT_ALLOWED, host, None).await
    /// });
    /// ```
    #[macro_export]
    macro_rules! prepare {
        ($request:ident, $host:ident, $path:ident, $addr:ident $(, move |$($clone:ident $(,)?)+|)? $code:block) => {
            $crate::extension!(|
                $request: RequestWrapperMut,
                $host: HostWrapper,
                $path: PathOptionWrapper |
                $addr: SocketAddr |,
                $($($clone)*)*,
                $code
            )
        }
    }
    /// Will make a present extension.
    ///
    /// See [`prepare!`] for usage and useful examples.
    ///
    /// # Examples
    /// ```
    /// # use kvarn::prelude::*;
    /// let extension = present!(data {
    ///     println!("Calling uri {}", data.request().uri());
    /// });
    /// ```
    #[macro_export]
    macro_rules! present {
        ($data:ident $(, move |$($clone:ident $(,)?)+|)? $code:block) => {
            extension!(|$data: PresentDataWrapper | |, $($($clone)*)*, $code)
        }
    }
    /// Will make a package extension.
    ///
    /// See [`prepare!`] for usage and useful examples.
    ///
    /// # Examples
    /// ```
    /// # use kvarn::prelude::*;
    /// let extension = package!(response, request, host {
    ///     response.headers_mut().insert("x-author", HeaderValue::from_static("Icelk"));
    ///     println!("Response headers {:#?}", response.headers());
    /// });
    /// ```
    #[macro_export]
    macro_rules! package {
        ($response:ident, $request:ident, $host:ident $(, move |$($clone:ident $(,)?)+|)? $code:block) => {
            extension!(|$response: EmptyResponseWrapperMut, $request: RequestWrapper, $host: HostWrapper | |, $($($clone)*)*, $code)
        }
    }
    /// Will make a post extension.
    ///
    /// See [`prepare!`] for usage and useful examples.
    ///
    /// # Examples
    /// ```
    /// # use kvarn::prelude::*;
    /// let extension = post!(request, host, response_pipe, bytes, addr {
    ///     match response_pipe {
    ///         application::ResponsePipe::Http1(c) => println!("This is a HTTP/1 connection. {:?}", c),
    ///         application::ResponsePipe::Http2(c) => println!("This is a HTTP/2 connection. {:?}", c),
    ///     }
    /// });
    /// ```
    #[macro_export]
    macro_rules! post {
        ($request:ident, $host:ident, $response_pipe:ident, $bytes:ident, $addr:ident $(, move |$($clone:ident $(,)?)+|)? $code:block) => {
            extension!(|$request: RequestWrapper, $host: HostWrapper, $response_pipe: ResponsePipeWrapperMut | $bytes: Bytes, $addr: SocketAddr|, $($($clone)*)*, $code)
        }
    }
    #[allow(unused_imports)]
    use super::ResponsePipeFuture;
    /// Creates a [`ResponsePipeFuture`].
    ///
    /// # Examples
    /// ```
    /// # use kvarn::prelude::*;
    /// prepare!(req, host, path, addr {
    ///     let response = default_error_response(StatusCode::METHOD_NOT_ALLOWED, host, None).await;
    ///     response.with_future(response_pipe_fut!(response_pipe, host {
    ///         response_pipe.send(Bytes::from_static(b"This will be appended to the body!")).await;
    ///     }))
    /// });
    /// ```
    #[macro_export]
    macro_rules! response_pipe_fut {
        ($response:ident, $host:ident $(, move |$($clone:ident $(,)?)+|)? $code:block) => {
            extension!(|$response: ResponseBodyPipeWrapperMut, $host: HostWrapper| |, $($($clone)*)*, $code)
        }
    }
}
