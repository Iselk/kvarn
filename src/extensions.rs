//! Here, all extensions code is housed.
//!
//! Check out [extensions.md](https://github.com/Icelk/kvarn/tree/main/extensions.md) for more info.
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
    (dyn Fn(RequestWrapperMut, HostWrapper, PathWrapper, SocketAddr) -> RetFut<FatResponse>
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
    (dyn Fn(RequestWrapper, Bytes, ResponsePipeWrapperMut, SocketAddr, HostWrapper) -> RetFut<()>
         + Sync
         + Send),
>;
/// Dynamic function to check if a extension should be ran.
///
/// Used with [`Prepare`] extensions
pub type If = Box<(dyn Fn(&FatRequest) -> bool + Sync + Send)>;
/// A [`Future`] for writing to a [`ResponsePipe`] after the response is sent.
///
/// Used with [`Prepare`] extensions
pub type ResponsePipeFuture = Box<
    dyn FnOnce(extensions::ResponseBodyPipeWrapperMut, extensions::HostWrapper) -> RetSyncFut<()>
        + Send
        + Sync,
>;

/// Magic number for [`Present`] extension.
///
/// `!> `
pub const PRESENT_INTERNAL_PREFIX: &[u8] = &[BANG, PIPE, SPACE];
/// Separator between [`Present`] extensions.
///
/// ` &> `
pub const PRESENT_INTERNAL_AND: &[u8] = &[SPACE, AMPERSAND, PIPE, SPACE];

/// Returns a future accepted by all the [`extensions`]
/// yielding immediately with `value`.
#[inline]
pub fn ready<T: 'static + Send>(value: T) -> RetFut<T> {
    Box::pin(core::future::ready(value))
}

macro_rules! order_reverse_by_first {
    ($list: expr) => {
        $list.sort_by(|a, b| b.0.cmp(&a.0));
    };
}

/// Contains all extensions.
/// See [extensions.md](../extensions.md) for more info.
///
/// `ToDo`: remove and list? Give mut access to underlying `Vec`s and `HashMap`s or a `Entry`-like interface?
#[allow(missing_debug_implementations)]
#[must_use]
pub struct Extensions {
    prime: Vec<(i32, Prime)>,
    prepare_single: HashMap<String, Prepare>,
    prepare_fn: Vec<(i32, If, Prepare)>,
    present_internal: HashMap<String, Present>,
    present_file: HashMap<String, Present>,
    package: Vec<(i32, Package)>,
    post: Vec<(i32, Post)>,
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
    pub fn new() -> Self {
        let mut new = Self::empty();

        new.add_prime(
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
            -100,
        );
        // new.add_package(
            // Box::new(|mut response, _, _| {
                // let response: &mut Response<()> = unsafe { response.get_inner() };
                // response
                    // .headers_mut()
                    // .entry("referrer-policy")
                    // .or_insert(HeaderValue::from_static("no-referrer"));

                // ready(())
            // }),
            // 10,
        // );

        new
    }
    /// Adds a prime extension. Higher `priority` extensions are ran first.
    pub fn add_prime(&mut self, extension: Prime, priority: i32) {
        self.prime.push((priority, extension));
        order_reverse_by_first!(self.prime);
    }
    /// Adds a prepare extension for a single URI.
    pub fn add_prepare_single(&mut self, path: String, extension: Prepare) {
        self.prepare_single.insert(path, extension);
    }
    /// Adds a prepare extension run if `function` return `true`. Higher `priority` extensions are ran first.
    pub fn add_prepare_fn(&mut self, predicate: If, extension: Prepare, priority: i32) {
        self.prepare_fn.push((priority, predicate, extension));
        order_reverse_by_first!(self.prepare_fn);
    }
    /// Adds a present internal extension, called with files starting with `!> `.
    pub fn add_present_internal(&mut self, name: String, extension: Present) {
        self.present_internal.insert(name, extension);
    }
    /// Adds a present file extension, called with file extensions matching `name`.
    pub fn add_present_file(&mut self, name: String, extension: Present) {
        self.present_file.insert(name, extension);
    }
    /// Adds a package extension, used to make last-minute changes to response. Higher `priority` extensions are ran first.
    pub fn add_package(&mut self, extension: Package, priority: i32) {
        self.package.push((priority, extension));
        order_reverse_by_first!(self.package);
    }
    /// Adds a post extension, used for HTTP/2 push Higher `priority` extensions are ran first.
    pub fn add_post(&mut self, extension: Post, priority: i32) {
        self.post.push((priority, extension));
        order_reverse_by_first!(self.post);
    }

    pub(crate) async fn resolve_prime(
        &self,
        request: &mut FatRequest,
        host: &Host,
        address: SocketAddr,
    ) {
        for (_, prime) in &self.prime {
            if let Some(prime) = prime(
                RequestWrapper::new(request),
                HostWrapper::new(host),
                address,
            )
            .await
            {
                *request.uri_mut() = prime;
            }
        }
    }
    pub(crate) async fn resolve_prepare(
        &self,
        request: &mut FatRequest,
        host: &Host,
        path: &Path,
        address: SocketAddr,
    ) -> Option<FatResponse> {
        if let Some(extension) = self.prepare_single.get(request.uri().path()) {
            Some(
                extension(
                    RequestWrapperMut::new(request),
                    HostWrapper::new(host),
                    PathWrapper::new(path),
                    address,
                )
                .await,
            )
        } else {
            for (_, function, extension) in &self.prepare_fn {
                if function(request) {
                    return Some(
                        extension(
                            RequestWrapperMut::new(request),
                            HostWrapper::new(host),
                            PathWrapper::new(path),
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
        let path = parse::uri(request.uri().path());

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
                Bytes::clone(&bytes),
                ResponsePipeWrapperMut::new(response_pipe),
                addr,
                HostWrapper::new(host),
            )
            .await;
        }
        if let Some((_, extension)) = self.post.last() {
            extension(
                RequestWrapper::new(request),
                bytes,
                ResponsePipeWrapperMut::new(response_pipe),
                addr,
                HostWrapper::new(host),
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

get_unsafe_wrapper!(RequestWrapper, FatRequest);
get_unsafe_mut_wrapper!(RequestWrapperMut, FatRequest);
get_unsafe_mut_wrapper!(EmptyResponseWrapperMut, Response<()>);
get_unsafe_mut_wrapper!(ResponsePipeWrapperMut, ResponsePipe);
get_unsafe_wrapper!(HostWrapper, Host);
get_unsafe_wrapper!(PathWrapper, Path);
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

#[derive(Debug)]
struct PresentExtensionPosData {
    name_start: usize,
    name_len: usize,

    arg_start: usize,
    arg_len: usize,
}
impl PresentExtensionPosData {
    fn from_name_and_arg(name: (usize, usize), arg: (usize, usize)) -> Self {
        Self {
            name_start: name.0,
            name_len: name.1,
            arg_start: arg.0,
            arg_len: arg.1,
        }
    }
    fn get_name(&self) -> (usize, usize) {
        (self.name_start, self.name_len)
    }
    fn get_arg(&self) -> (usize, usize) {
        (self.arg_start, self.arg_len)
    }
}

/// The [`Present`] extensions parsed from a file containing them.
#[allow(clippy::module_name_repetitions)]
#[derive(Debug, Clone)]
#[must_use]
pub struct PresentExtensions {
    data: Bytes,
    extensions: Arc<Vec<PresentExtensionPosData>>,
    data_start: usize,
}
impl PresentExtensions {
    /// Parses a file to create a representation of the [`Present`] extensions in it.
    ///
    /// `data` should start with [`PRESENT_INTERNAL_PREFIX`], as all present extension files should.
    pub fn new(data: Bytes) -> Option<Self> {
        let mut extensions_args =
            Vec::with_capacity(
                data.iter()
                    .fold(1, |acc, byte| if *byte == SPACE { acc + 1 } else { acc }),
            );

        if !data.starts_with(PRESENT_INTERNAL_PREFIX)
            || data[PRESENT_INTERNAL_PREFIX.len()..].starts_with(PRESENT_INTERNAL_AND)
        {
            return None;
        }
        let mut start = PRESENT_INTERNAL_PREFIX.len();
        let mut last_name = None;
        let mut has_cr = false;
        for (pos, byte) in data.iter().enumerate().skip(3) {
            if start > pos {
                continue;
            }
            let byte = *byte;

            if byte == SPACE || byte == CR || byte == LF {
                if str::from_utf8(&data[start..pos]).is_err() {
                    return None;
                }
                let len = pos - start;
                let span = (start, len);

                // We have to borrow same mutably, which isn't possible in closures.
                #[allow(clippy::option_if_let_else)]
                if let Some(name) = last_name {
                    extensions_args.push(PresentExtensionPosData::from_name_and_arg(name, span))
                } else {
                    last_name = Some((start, len));
                    extensions_args.push(PresentExtensionPosData::from_name_and_arg(span, span))
                }
                if byte == CR {
                    has_cr = true;
                }
                if byte == CR || byte == LF {
                    return Some(Self {
                        data,
                        extensions: Arc::new(extensions_args),
                        data_start: pos + if has_cr { 2 } else { 1 },
                    });
                }
                start = if data[pos..].starts_with(PRESENT_INTERNAL_AND) {
                    last_name = None;
                    pos + PRESENT_INTERNAL_AND.len()
                } else {
                    pos + 1
                };
            }
        }

        None
    }
    /// Creates an empty representation of [`Present`] extensions
    pub fn empty() -> Self {
        Self {
            data: Bytes::new(),
            extensions: Arc::new(Vec::new()),
            data_start: 0,
        }
    }
    /// Gets an iterator of self.
    ///
    /// Clones the inner data.
    #[inline]
    pub fn iter(&self) -> PresentExtensionsIter {
        PresentExtensionsIter {
            data: Self::clone(&self),
            index: 0,
        }
    }
    /// Returns the start of the document data, after all extensions and their arguments.
    #[inline]
    pub fn data_start(&self) -> usize {
        self.data_start
    }
}
impl IntoIterator for PresentExtensions {
    type Item = PresentArguments;
    type IntoIter = PresentExtensionsIter;
    fn into_iter(self) -> Self::IntoIter {
        PresentExtensionsIter {
            data: self,
            index: 0,
        }
    }
}
/// An iterator of [`PresentArguments`] from [`PresentExtensions`]
#[derive(Debug)]
pub struct PresentExtensionsIter {
    data: PresentExtensions,
    index: usize,
}
impl Iterator for PresentExtensionsIter {
    type Item = PresentArguments;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        let start = self.index;
        if start == self.data.extensions.len() {
            return None;
        }
        let name = self.data.extensions[start].get_name();

        let iter = self.data.extensions[start + 1..].iter();

        for current in iter {
            self.index += 1;
            if current.get_name() != name {
                break;
            }
        }
        // Cannot change name ↑ on last item; the end of each *peeks* forward one. If it's next to the end, add one.
        if self.index + 1 == self.data.extensions.len() {
            self.index += 1
        };
        Some(PresentArguments {
            data: PresentExtensions::clone(&self.data),
            data_index: start,
            len: self.index - start,
        })
    }
}
/// The arguments and name of a single [`Present`] extension.
#[derive(Debug)]
#[must_use]
pub struct PresentArguments {
    data: PresentExtensions,
    data_index: usize,
    len: usize,
}
impl PresentArguments {
    /// Creates an empty representation of [`Present`] arguments
    #[inline]
    pub fn empty() -> Self {
        Self {
            data: PresentExtensions::empty(),
            data_index: 0,
            len: 0,
        }
    }
    /// Gets the name of the extension.
    #[inline]
    pub fn name(&self) -> &str {
        // .1 and .0 should be the same; the name of (usize, usize) should have the same name as it's first argument.
        let (start, len) = self.data.extensions[self.data_index].get_name();
        // safe, because we checked for str in creation of [`PresentExtensions`].
        unsafe { str::from_utf8_unchecked(&self.data.data[start..start + len]) }
    }
    /// Returns an iterator of the arguments as [`prim@str`]s.
    #[inline]
    pub fn iter(&self) -> PresentArgumentsIter<'_> {
        PresentArgumentsIter {
            data: &self.data,
            data_index: self.data_index,
            back_index: self.len,
            index: 1,
        }
    }
}
/// An iterator of [`prim@str`] for the arguments in [`PresentArguments`]
#[derive(Debug)]
pub struct PresentArgumentsIter<'a> {
    data: &'a PresentExtensions,
    data_index: usize,
    back_index: usize,
    index: usize,
}
impl<'a> Iterator for PresentArgumentsIter<'a> {
    type Item = &'a str;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        if self.index == self.back_index {
            return None;
        }
        let (start, len) = self.data.extensions[self.data_index + self.index].get_arg();
        self.index += 1;
        // Again, safe because we checked for str in creation of [`PresentExtensions`].
        Some(unsafe { str::from_utf8_unchecked(&self.data.data[start..start + len]) })
    }
}
impl<'a> DoubleEndedIterator for PresentArgumentsIter<'a> {
    #[inline]
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.index == self.back_index {
            return None;
        }
        let (start, len) = self.data.extensions[self.data_index + self.back_index - 1].get_arg();
        self.back_index -= 1;
        // Again, safe because we checked for str in creation of [`PresentExtensions`].
        Some(unsafe { str::from_utf8_unchecked(&self.data.data[start..start + len]) })
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
    ///     path: PathWrapper |
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
                $(let $clone = Arc::clone(&$clone);)*
                Box::pin(async move {
                    $(let $wrapper_param = unsafe { $wrapper_param.get_inner() };)*

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
    ///     utility::default_error_response(StatusCode::BAD_REQUEST, host, None).await
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
    ///     utility::default_error_response(StatusCode::NOT_FOUND, host, None).await
    /// });
    /// ```
    ///
    /// To capture no variables, just leave out the `move ||`.
    /// ```
    /// # use kvarn::prelude::*;
    /// prepare!(req, host, path, addr {
    ///     utility::default_error_response(StatusCode::METHOD_NOT_ALLOWED, host, None).await
    /// });
    /// ```
    #[macro_export]
    macro_rules! prepare {
        ($request:ident, $host:ident, $path:ident, $addr:ident $(, move |$($clone:ident $(,)?)+|)? $code:block) => {
            $crate::extension!(|
                $request: RequestWrapperMut,
                $host: HostWrapper,
                $path: PathWrapper |
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
    /// let extension = post!(request, bytes, response, address, host {
    ///     let valid_utf8 = response.headers().get("content-type").map(HeaderValue::to_str)
    ///         .and_then(Result::ok).map(|s| s.contains("utf8")).unwrap_or(false);
    ///     
    ///     match valid_utf8 {
    ///         true => match str::from_utf8(&bytes) {
    ///             Ok(s) => println!("Sent response in cleartext: '{}'", s),
    ///             Err(_) => println!("Response is UTF-8, but the bytes are not. Probably compressed."),
    ///         },
    ///         false => println!("Response is not UTF-8."),
    ///     }
    /// });
    /// ```
    #[macro_export]
    macro_rules! post {
        ($request:ident, $bytes:ident, $response:ident, $addr:ident, $host:ident $(, move |$($clone:ident $(,)?)+|)? $code:block) => {
            extension!(|$request: RequestWrapper, $response: EmptyResponseWrapperMut, $host: HostWrapper | $bytes: Bytes, $addr: SocketAddr|, $($($clone)*)*, $code)
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
    ///     let response = utility::default_error_response(StatusCode::METHOD_NOT_ALLOWED, host, None).await;
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
