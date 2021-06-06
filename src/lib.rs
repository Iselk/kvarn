//! An extensible and efficient forward-thinking web server for the future.
//!
//! Kvarn is a rethought web server tailored for the current needs from web application developers.
//!
//! It handles several things for you, including
//! - Content-Type
//! - Compression of body
//! - Correct and performant HTTP/1 and HTTP/2
//! - Common API across HTTP/1 and HTTP/2
//! - Easy integration with HTTP/2 push promises
//! - Five types of extensions, all backed with intuitive macros
//! - Optional encryption with [`rustls`]
//! - Several checks for illegal requests
//! - `cache-control` and [`kvarn-cache-control`](parse::CacheControl::from_kvarn_cache_control) header limits server cache lifetimes
//!
//! # Getting started
//!
//! The main function to call is [`run`]. See the example at [`run`]
//! on how to get a simple web server running.
//!
//! A battle-tested reference implementation can be found at [GitHub](https://github.com/Icelk/kvarn-reference/).
//! It powers my two websites with minimal resource requirements.
//!
//! # Future plans
//!
//! See the [README @ GitHub](https://github.com/Icelk/kvarn/) and [kvarn.org](https://kvarn.org).
#![deny(
    unreachable_pub,
    missing_debug_implementations,
    missing_docs,
    clippy::pedantic
)]
#![allow(
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    // when a parameter of a function is prefixed due to cfg in fn
    clippy::used_underscore_binding,
    // same as ↑
    clippy::unused_self,
    // When a enum variant has been conditionally compiled away
    irrefutable_let_patterns,
)]
#![doc(html_favicon_url = "https://kvarn.org/favicon.png")]
#![doc(html_logo_url = "https://kvarn.org/logo.svg")]
#![doc(html_root_url = "https://doc.kvarn.org/")]

// Module declaration
pub mod application;
pub mod comprash;
pub mod encryption;
pub mod extensions;
pub mod host;
pub mod limiting;
pub mod parse;
pub mod prelude;
pub mod shutdown;
pub mod utility;

use prelude::{internals::*, networking::*, *};
// When user only imports crate::* and not crate::prelude::*
pub use comprash::{
    ClientCachePreference, CompressPreference, CompressedResponse, ServerCachePreference,
};
pub use extensions::Extensions;
pub use utility::{default_error, default_error_response, read_file, read_file_cached};
/// The `Request` used within Kvarn.
pub type FatRequest = Request<application::Body>;
/// A `Response` returned by [`handle_request()`].
///
/// Contains all preference information to the lower-level
/// functions. Most things like `content-length`, `content-encoding`,
/// `content-type`, `cache-control`, and server caching will be
/// automatically handled.
pub struct FatResponse {
    response: Response<Bytes>,
    client: ClientCachePreference,
    server: ServerCachePreference,
    compress: CompressPreference,

    future: Option<ResponsePipeFuture>,
}
impl FatResponse {
    /// Creates a new [`FatResponse`] with `server_cache_preference` advising Kvarn of how to cache the content.
    ///
    /// Choose
    /// - [`ServerCachePreference::Full`] if the page is one regularly accessed,
    /// - [`ServerCachePreference::None`] if the page is rarely accessed or if the runtime cost of
    ///   getting the page is minimal.
    /// - [`ServerCachePreference::QueryMatters`] should be avoided. It should be used when
    ///   you have a page dictated by the query. Consider using a [`Prime`] extension
    ///   to make all requests act as only one of a few queries to increase performance
    ///   by reducing cache size.
    pub fn new(response: Response<Bytes>, server_cache_preference: ServerCachePreference) -> Self {
        Self {
            response,
            client: ClientCachePreference::Full,
            server: server_cache_preference,
            compress: CompressPreference::Full,

            future: None,
        }
    }
    /// Creates a new [`FatResponse`] with all preferences set to `Full` and no `Future`.
    ///
    /// Use the `with_*` methods to change the defaults.
    pub fn cache(response: Response<Bytes>) -> Self {
        Self::new(response, ServerCachePreference::Full)
    }
    /// Creates a new [`FatResponse`] with all cache preferences set to `None`,
    /// compress preference set to `Full`, and no `Future`.
    ///
    /// Use the `with_*` methods to change the defaults.
    pub fn no_cache(response: Response<Bytes>) -> Self {
        Self {
            response,
            client: ClientCachePreference::None,
            server: ServerCachePreference::None,
            compress: CompressPreference::Full,
            future: None,
        }
    }
    /// Sets the inner [`ClientCachePreference`].
    pub fn with_client_cache(mut self, preference: ClientCachePreference) -> Self {
        self.client = preference;
        self
    }
    /// Sets the inner [`ServerCachePreference`].
    pub fn with_server_cache(mut self, preference: ServerCachePreference) -> Self {
        self.server = preference;
        self
    }
    /// Sets the inner [`CompressPreference`].
    pub fn with_compress(mut self, preference: CompressPreference) -> Self {
        self.compress = preference;
        self
    }
    /// Sets the inner `Future`.
    pub fn with_future(mut self, future: ResponsePipeFuture) -> Self {
        self.future = Some(future);
        self
    }
    /// Turns `self` into a tuple of all it's parts.
    pub fn into_parts(
        self,
    ) -> (
        Response<Bytes>,
        ClientCachePreference,
        ServerCachePreference,
        CompressPreference,
        Option<ResponsePipeFuture>,
    ) {
        (
            self.response,
            self.client,
            self.server,
            self.compress,
            self.future,
        )
    }
}
impl Debug for FatResponse {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        #[derive(Debug)]
        enum BytesOrStr<'a> {
            Str(&'a str),
            Bytes(&'a [u8]),
        }
        let response = utility::empty_clone_response(&self.response);
        let body = if let Ok(s) = str::from_utf8(self.response.body()) {
            BytesOrStr::Str(s)
        } else {
            BytesOrStr::Bytes(self.response.body())
        };
        let response = response.map(|()| body);
        f.debug_struct("FatResponse")
            .field("resp", &response)
            .field("client", &self.client)
            .field("server", &self.server)
            .field("compress", &self.compress)
            .field("future", &"opaque Future".as_clean())
            .finish()
    }
}

/// The Kvarn `server` header.
/// Can also be used for identifying the client when using
/// Kvarn as a reverse-proxy.
#[cfg(target_os = "windows")]
pub const SERVER: &str = "Kvarn/0.2.0 (Windows)";
/// The Kvarn `server` header.
/// Can also be used for identifying the client when using
/// Kvarn as a reverse-proxy.
#[cfg(target_os = "macos")]
pub const SERVER: &str = "Kvarn/0.2.0 (macOS)";
/// The Kvarn `server` header.
/// Can also be used for identifying the client when using
/// Kvarn as a reverse-proxy.
#[cfg(target_os = "linux")]
pub const SERVER: &str = "Kvarn/0.2.0 (Linux)";
/// The Kvarn `server` header.
/// Can also be used for identifying the client when using
/// Kvarn as a reverse-proxy.
#[cfg(target_os = "freebsd")]
pub const SERVER: &str = "Kvarn/0.2.0 (FreeBSD)";
/// The Kvarn `server` header.
/// Can also be used for identifying the client when using
/// Kvarn as a reverse-proxy.
#[cfg(not(any(
    target_os = "windows",
    target_os = "macos",
    target_os = "linux",
    target_os = "freebsd"
)))]
pub const SERVER: &str = "Kvarn/0.2.0 (unknown OS)";

/// All the supported ALPN protocols.
///
/// > ***Note:** this is often not needed, as the ALPN protocols
/// are set in [`host::Data::make_config()`].*
#[must_use]
pub fn alpn() -> Vec<Vec<u8>> {
    #[allow(unused_mut)]
    let mut vec = Vec::with_capacity(4);
    #[cfg(feature = "http2")]
    {
        vec.push(b"h2".to_vec());
    }
    vec
}

macro_rules! ret_log_app_error {
    ($e:expr) => {
        match $e {
            Err(err) => {
                error!("An error occurred while sending a request. {:?}", &err);
                return Err(err.into());
            }
            Ok(val) => val,
        }
    };
}

/// Handles a single connection. This includes encrypting it, extracting the HTTP header information,
/// optionally (HTTP/2 & HTTP/3) decompressing them, and passing the request to [`handle_cache()`].
/// It will also recognise which host should handle the connection.
///
/// # Errors
///
/// Will pass any errors from reading the request, making a TLS handshake, and writing the response.
/// See [`handle_cache()`] and [`handle_request()`]; errors from them are passed up, through this fn.
pub async fn handle_connection(
    stream: TcpStream,
    address: SocketAddr,
    descriptors: Arc<PortDescriptor>,
    #[allow(unused_variables)] limiter: LimitWrapper,
    mut continue_accepting: impl FnMut() -> bool,
) -> io::Result<()> {
    #[cfg(feature = "limiting")]
    let mut limiter = limiter;

    // LAYER 2
    #[cfg(feature = "https")]
    let encrypted =
        encryption::Encryption::new_tcp(stream, descriptors.server_config.as_ref()).await?;
    #[cfg(not(feature = "https"))]
    let encrypted = encryption::Encryption::new_tcp(stream);

    let version =
        match encrypted.get_alpn_protocol() {
            Some(b"h2") => Version::HTTP_2,
            None | Some(b"http/1.1") => Version::HTTP_11,
            Some(b"http/1.0") => Version::HTTP_10,
            Some(b"http/0.9") => Version::HTTP_09,
            _ => return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP version not supported. Something is probably wrong with your alpn config.",
            )),
        };
    let hostname = encrypted.get_sni_hostname().map(str::to_string);
    debug!("New connection requesting hostname '{:?}'", hostname);

    // LAYER 3
    let mut http = application::HttpConnection::new(encrypted, version)
        .await
        .map_err::<io::Error, _>(application::Error::into)?;

    info!("Accepting requests from {}", address);

    while let Ok((request, mut response_pipe)) = http
        .accept(descriptors.data.get_default().name.as_bytes())
        .await
    {
        trace!("Got request {:#?}", request);
        #[cfg(feature = "limiting")]
        match limiter.register(address.ip()).await {
            LimitStrength::Drop => return Ok(()),
            LimitStrength::Send => {
                let version = match response_pipe {
                    ResponsePipe::Http1(_) => Version::HTTP_11,
                    ResponsePipe::Http2(_) => Version::HTTP_2,
                };
                let (mut response, body) =
                    utility::split_response(limiting::get_too_many_requests());
                *response.version_mut() = version;
                let mut body_pipe =
                    ret_log_app_error!(response_pipe.send_response(response, false).await);
                ret_log_app_error!(body_pipe.send_with_maybe_close(body, true).await);
                continue;
            }
            LimitStrength::Passed => {}
        }
        let host = descriptors.data.smart_get(&request, hostname.as_deref());
        debug!("Accepting new request from {} on {}", address, host.name);
        // fn to handle getting from cache, generating response and sending it
        handle_cache(request, address, SendKind::Send(&mut response_pipe), host).await?;

        if !continue_accepting() {
            break;
        }
    }

    Ok(())
}

/// How to send data to the client.
///
/// Most often, this is `Send`, but when a push promise is created,
/// this will be `Push`. This can be used by [`extensions::Post`].
#[derive(Debug)]
pub enum SendKind<'a> {
    /// Send the response normally.
    Send(&'a mut application::ResponsePipe),
    /// Send the response as a HTTP/2 push.
    Push(&'a mut application::PushedResponsePipe),
}
impl<'a> SendKind<'a> {
    /// Ensures correct version and length (only applicable for HTTP/1 connections)
    /// of a response according to inner enum variants.
    #[inline]
    pub fn ensure_version_and_length<T>(&self, response: &mut Response<T>, len: usize) {
        match self {
            Self::Send(p) => p.ensure_version_and_length(response, len),
            Self::Push(p) => p.ensure_version_and_length(response, len),
        }
    }
    #[inline]
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn send<F: Future<Output = ()>>(
        &mut self,
        mut response: Response<Bytes>,
        identity_body: Bytes,
        request: &FatRequest,
        host: &Host,
        future: Option<
            impl FnOnce(extensions::ResponseBodyPipeWrapperMut, extensions::HostWrapper) -> F,
        >,
        address: SocketAddr,
        data: Option<parse::CriticalRequestComponents>,
    ) -> io::Result<()> {
        if let Some(data) = &data {
            data.apply_to_response(&mut response).await;
        }

        let len = response.body().len();
        self.ensure_version_and_length(&mut response, len);

        let (mut response, body) = utility::split_response(response);

        host.extensions
            .resolve_package(&mut response, request, host)
            .await;

        match self {
            SendKind::Send(response_pipe) => {
                // Process post extensions
                host.extensions
                    .resolve_post(&request, identity_body, response_pipe, address, host)
                    .await;

                // Send response
                let mut body_pipe =
                    ret_log_app_error!(response_pipe.send_response(response, false).await);

                if utility::method_has_response_body(request.method()) {
                    // Send body
                    ret_log_app_error!(body_pipe.send_with_maybe_close(body, false).await);
                }

                if let Some(future) = future {
                    future(
                        extensions::ResponseBodyPipeWrapperMut::new(&mut body_pipe),
                        extensions::HostWrapper::new(host),
                    )
                    .await;
                }

                // Close the pipe.
                ret_log_app_error!(body_pipe.close().await);
            }
            SendKind::Push(push_pipe) => {
                let send_body = utility::method_has_response_body(request.method());

                // Send response
                let mut body_pipe = ret_log_app_error!(
                    push_pipe.send_response(response, !send_body && future.is_none())
                );
                if send_body {
                    // Send body
                    ret_log_app_error!(
                        body_pipe
                            .send_with_maybe_close(body, false && future.is_none())
                            .await
                    );
                }
                if let Some(future) = future {
                    future(
                        extensions::ResponseBodyPipeWrapperMut::new(&mut body_pipe),
                        extensions::HostWrapper::new(host),
                    )
                    .await;
                }
                ret_log_app_error!(body_pipe.close().await);

                if !send_body {
                    ret_log_app_error!(body_pipe.close().await);
                }
            }
        }
        Ok(())
    }
}

/// Will handle a single request, check the cache, process if needed, and caches it.
/// This is where the response is sent.
///
/// # Errors
///
/// Errors are passed from writing the response.
///
/// LAYER 4
pub async fn handle_cache(
    mut request: Request<application::Body>,
    address: SocketAddr,
    mut pipe: SendKind<'_>,
    host: &Host,
) -> io::Result<()> {
    let sanitize_data = parse::sanitize_request(&request);

    host.extensions
        .resolve_prime(&mut request, host, address)
        .await;

    let mut path_query = comprash::UriKey::path_and_query(request.uri());

    let mut lock = host.response_cache.lock().await;
    // copy of [`UriKey::call_all`].
    // I got the message
    // ```
    // captured variable cannot escape `FnMut` closure body
    // `FnMut` closures only have access to their captured variables while they are executing...
    // ...therefore, they cannot allow references to captured variables to escape
    // ```
    // and had to inline it.
    let cached = match lock.get(&path_query).into_option() {
        Some(t) => Some(t),
        None => match &mut path_query {
            UriKey::Path(_) => None,
            UriKey::PathQuery(p) => {
                p.truncate_query();
                let t = lock.get(&path_query).into_option();
                t
            }
        },
    };
    #[allow(clippy::single_match_else)]
    let (response, identity, future) = match cached {
        Some(resp) if sanitize_data.is_ok() => {
            info!("Found in cache!");
            let response = match resp.clone_preferred(&request) {
                Err(message) => {
                    utility::default_error(
                        StatusCode::NOT_ACCEPTABLE,
                        Some(host),
                        Some(message.as_bytes()),
                    )
                    .await
                }
                Ok(response) => response,
            };
            let identity_body = Bytes::clone(resp.get_identity().body());
            drop(lock);

            (response, identity_body, None)
        }
        _ => {
            async fn maybe_cache<T>(
                host: &Host,
                server_cache: ServerCachePreference,
                path_query: PathQuery,
                response: CompressedResponse,
                future: &Option<T>,
            ) {
                if future.is_none() {
                    if server_cache.cache(response.get_identity()) {
                        let mut lock = host.response_cache.lock().await;
                        let key = if server_cache.query_matters() {
                            comprash::UriKey::PathQuery(path_query)
                        } else {
                            comprash::UriKey::Path(path_query.into_path())
                        };
                        info!("Caching uri {:?}!", &key);
                        lock.cache(key, response);
                    }
                } else {
                    info!("Not caching; a Prepare extension has captured. If we cached, it would not be called again.");
                }
            }

            drop(lock);
            let path_query = comprash::PathQuery::from_uri(request.uri());
            // LAYER 5.1
            let (mut resp, mut client_cache, mut server_cache, compress, future) =
                match sanitize_data.as_ref() {
                    Ok(_) => {
                        let path = utility::make_path(
                            &host.path,
                            host.options
                                .public_data_dir
                                .as_deref()
                                .unwrap_or_else(|| Path::new("public")),
                            // Ok, since `path.is_some()` above which calls the same function.
                            parse::uri(request.uri().path()).unwrap(),
                            None,
                        );

                        handle_request(&mut request, address, host, &path).await?
                    }
                    Err(err) => err.into_response(host).await,
                }
                .into_parts();

            host.extensions
                .resolve_present(
                    &mut request,
                    &mut resp,
                    &mut client_cache,
                    &mut server_cache,
                    host,
                    address,
                )
                .await?;

            let extension = match Path::new(request.uri().path())
                .extension()
                .and_then(std::ffi::OsStr::to_str)
            {
                Some(ext) => ext,
                None => match host.options.extension_default.as_ref() {
                    Some(ext) => ext.as_str(),
                    None => "",
                },
            };
            let compressed_response = comprash::CompressedResponse::new(
                resp,
                compress,
                client_cache,
                extension,
                host.options.disable_client_cache,
            );

            let response = match compressed_response.clone_preferred(&request) {
                Err(message) => {
                    utility::default_error(
                        StatusCode::NOT_ACCEPTABLE,
                        Some(host),
                        Some(message.as_bytes()),
                    )
                    .await
                }
                Ok(response) => response,
            };

            let identity_body = Bytes::clone(compressed_response.get_identity().body());

            maybe_cache(host, server_cache, path_query, compressed_response, &future).await;

            (response, identity_body, future)
        }
    };
    pipe.send(
        response,
        identity,
        &request,
        host,
        future,
        address,
        sanitize_data.ok(),
    )
    .await?;

    Ok(())
}

/// Handles a single request and returns response with cache and compress preference.
///
///  
/// # Errors
///
/// ~~Will return any errors from reading from the body of `request`.~~ Currently, does not return any errors.
///
///  
/// LAYER 5.1
pub async fn handle_request(
    request: &mut Request<application::Body>,
    address: net::SocketAddr,
    host: &Host,
    path: &Path,
) -> io::Result<FatResponse> {
    let mut response = None;
    let mut client_cache = None;
    let mut server_cache = None;
    let mut compress = None;
    let mut future = None;

    #[allow(unused_mut)]
    let mut status = None;

    {
        if let Some(resp) = host
            .extensions
            .resolve_prepare(request, &host, path, address)
            .await
        {
            let resp = resp.into_parts();
            response.replace(resp.0);
            client_cache.replace(resp.1);
            server_cache.replace(resp.2);
            compress.replace(resp.3);
            if let Some(f) = resp.4 {
                future.replace(f);
            }
        }
    }

    #[cfg(feature = "fs")]
    if response.is_none() {
        match *request.method() {
            Method::GET | Method::HEAD => {
                if let Some(content) = utility::read_file(&path, &host.file_cache).await {
                    response = Some(Response::new(content));
                }
            }
            _ => status = Some(StatusCode::METHOD_NOT_ALLOWED),
        }
    }

    let response = match response {
        Some(r) => r,
        None => {
            utility::default_error_response(status.unwrap_or(StatusCode::NOT_FOUND), host, None)
                .await
                .response
        }
    };

    macro_rules! maybe_with {
        ($response: expr, $option: expr, $method: tt) => {
            if let Some(t) = $option {
                $response = $response.$method(t);
            }
        };
    }
    let mut response = FatResponse::cache(response);
    maybe_with!(response, client_cache, with_client_cache);
    maybe_with!(response, server_cache, with_server_cache);
    maybe_with!(response, compress, with_compress);
    maybe_with!(response, future, with_future);

    Ok(response)
}

/// Describes port, certificate, and host data for
/// a single port to bind.
#[derive(Clone)]
#[must_use]
pub struct PortDescriptor {
    port: u16,
    #[cfg(feature = "https")]
    server_config: Option<Arc<rustls::ServerConfig>>,
    data: Arc<Data>,
}
impl PortDescriptor {
    /// Uses the defaults for non-secure HTTP with `host_data`
    pub fn http(host_data: Arc<Data>) -> Self {
        Self {
            port: 80,
            #[cfg(feature = "https")]
            server_config: None,
            data: host_data,
        }
    }
    /// Uses the defaults for secure HTTP, HTTPS, with `host_data`.
    /// Gets a [`rustls::ServerConfig`] from [`Data::make_config()`].
    #[cfg(feature = "https")]
    pub fn https(host_data: Arc<Data>) -> Self {
        Self {
            port: 443,
            server_config: Some(Arc::new(host_data.make_config())),
            data: host_data,
        }
    }
    /// Creates a new descriptor for `port` with `host_data` and an optional [`rustls::ServerConfig`].
    #[cfg(feature = "https")]
    pub fn with_server_config(
        port: u16,
        host_data: Arc<Data>,
        server_config: Option<Arc<rustls::ServerConfig>>,
    ) -> Self {
        Self {
            port,
            server_config,
            data: host_data,
        }
    }
    /// Creates a new descriptor for `port` with `host_data`.
    /// If the feature `https` is enabled, a `rustls::ServerConfig` is created
    /// from the `host_data`.
    pub fn new(port: u16, host_data: Arc<Data>) -> Self {
        Self {
            port,
            #[cfg(feature = "https")]
            server_config: Some(Arc::new(host_data.make_config())),
            data: host_data,
        }
    }
    /// Creates a new non-secure descriptor for `port` with `host_data`.
    /// Does not try to assign a certificate.
    pub fn non_secure(port: u16, host_data: Arc<Data>) -> Self {
        Self {
            port,
            #[cfg(feature = "https")]
            server_config: None,
            data: host_data,
        }
    }
}
impl Debug for PortDescriptor {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let mut s = f.debug_struct("HostDescriptor");
        s.field("port", &self.port);

        #[cfg(feature = "https")]
        s.field(
            "server_config",
            &self
                .server_config
                .as_ref()
                .map(|_| "certificate".as_clean()),
        );

        s.field("host_data", &self.data).finish()
    }
}

/// Run the Kvarn web server on `ports`.
///
/// Will bind a [`TcpListener`] on every `port` in [`PortDescriptor`].
///
/// > This ↑ will change when HTTP/3 support arrives, then Udp will also be used.
///
/// This is the last step in getting Kvarn spinning.
/// You can interact with the caches through the [`Host`] and [`Data`] you created, and
/// the returned [`shutdown::Manager`], if you have the `graceful-shutdown` feature enabled.
///
/// # Examples
///
/// Will start a bare-bones web server on port `8080`, using the dir `web` to serve files.
///
/// > **Note:** it uses `web` to serve files only if the feature `fs` is enabled. Place them in `web/public`
/// > to access them in your user-agent.
/// > It's done this way to enable you to have domain-specific files not being public to the web,
/// > and for a place to store other important files. Kvarn extensions' template system will in this case
/// > read template files from `web/templates`.
///
/// ```no_run
/// use kvarn::prelude::*;
///
/// # async {
/// // Create a host with hostname "localhost", serving files from directory "./web/public/", with the default extensions and the default options.
/// let host = Host::non_secure("localhost", PathBuf::from("web"), Extensions::default(), host::Options::default());
/// // Create a set of virtual hosts (`Data`) with `host` as the default.
/// let data = Data::builder(host).build();
/// // Bind port 8080 with `data`.
/// let port_descriptor = PortDescriptor::new(8080, data);
///
/// // Run with the configured ports.
/// let shutdown_manager = run(vec![port_descriptor]).await;
/// // Waits for shutdown.
/// shutdown_manager.wait().await;
/// # };
/// ```
pub async fn run(ports: Vec<PortDescriptor>) -> Arc<shutdown::Manager> {
    info!("Starting server on {} ports.", ports.len());

    let len = ports.len();
    let mut shutdown_manager = shutdown::Manager::new(len);

    let mut listeners = Vec::with_capacity(len);
    for descriptor in ports {
        let socket = TcpSocket::new_v4().expect("Failed to create a new IPv4 socket configuration");
        #[cfg(all(unix, not(target_os = "solaris"), not(target_os = "illumos")))]
        {
            if socket.set_reuseaddr(true).is_err() || socket.set_reuseport(true).is_err() {
                error!("Failed to set reuse address/port. This is needed for graceful shutdown handover.")
            }
        }
        socket
            .bind(net::SocketAddrV4::new(net::Ipv4Addr::UNSPECIFIED, descriptor.port).into())
            .expect("Failed to bind address");

        let listener = socket
            .listen(1024)
            .expect("Failed to listen on bound address.");

        let listener = shutdown_manager.add_listener(listener);
        listeners.push((listener, descriptor));
    }

    let shutdown_manager = shutdown_manager.build();

    for (listener, descriptor) in listeners {
        let shutdown_manager = Arc::clone(&shutdown_manager);
        let future = async move {
            accept(listener, descriptor, &shutdown_manager)
                .await
                .expect("Failed to accept message!")
        };

        tokio::spawn(future);
    }

    shutdown_manager
}

async fn accept(
    mut listener: AcceptManager,
    descriptor: PortDescriptor,
    shutdown_manager: &Arc<shutdown::Manager>,
) -> Result<(), io::Error> {
    trace!(
        "Started listening on {:?}",
        listener.get_inner().local_addr()
    );
    let host = Arc::new(descriptor);

    #[allow(unused_mut)]
    let mut limiter = LimitWrapper::default();

    loop {
        match listener.accept(shutdown_manager).await {
            AcceptAction::Shutdown => return Ok(()),
            AcceptAction::Accept(result) => match result {
                Ok((socket, addr)) => {
                    #[cfg(feature = "limiting")]
                    match limiter.register(addr.ip()).await {
                        LimitStrength::Drop => {
                            drop(socket);
                            return Ok(());
                        }
                        LimitStrength::Send | LimitStrength::Passed => {}
                    }
                    let host = Arc::clone(&host);
                    let limiter = LimitWrapper::clone(&limiter);
                    #[cfg(feature = "graceful-shutdown")]
                    let shutdown_manager = Arc::clone(shutdown_manager);
                    tokio::spawn(async move {
                        #[cfg(feature = "graceful-shutdown")]
                        shutdown_manager.add_connection();
                        if let Err(err) = handle_connection(socket, addr, host, limiter, || {
                            #[cfg(feature = "graceful-shutdown")]
                            {
                                !shutdown_manager.get_shutdown(threading::Ordering::Relaxed)
                            }
                            #[cfg(not(feature = "graceful-shutdown"))]
                            {
                                true
                            }
                        })
                        .await
                        {
                            warn!(
                                "An error occurred in the main processing function {:?}",
                                err
                            );
                        }
                        #[cfg(feature = "graceful-shutdown")]
                        shutdown_manager.remove_connection();
                    });
                    continue;
                }
                Err(err) => {
                    // An error occurred
                    error!("Failed to accept() on listener");

                    return Err(err);
                }
            },
        }
    }
}
