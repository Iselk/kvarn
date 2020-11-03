use http;
use mime::Mime;
use mime_guess;
use mio::net::{TcpListener, TcpStream};
use std::borrow::Cow;
use std::net;
use std::path::PathBuf;
use std::sync::{self, Arc, Mutex};
use std::{
    fs::File,
    io::{self, prelude::*},
};

pub mod extension_helper;
mod extensions;
pub mod parse;
mod threading;

pub use bindings::{ContentType, FunctionBindings};
pub use cache::types::*;
pub use cache::ByteResponse;
pub use chars::*;
pub use connection::Connection;
pub use extension_helper::*;

const HTTPS_SERVER: mio::Token = mio::Token(0);
const RESERVED_TOKENS: usize = 1024;
#[cfg(windows)]
pub const SERVER_HEADER: &[u8] = b"Server: Arktis/0.1.0 (Windows)\r\n";
#[cfg(unix)]
pub const SERVER_HEADER: &[u8] = b"Server: Arktis/0.1.0 (Unix)\r\n";
pub const SERVER_NAME: &str = "Arktis";
pub const LINE_ENDING: &[u8] = b"\r\n";

pub mod chars {
    /// Line feed
    pub const LF: u8 = 10;
    /// Carrage return
    pub const CR: u8 = 13;
    /// ` `
    pub const SPACE: u8 = 32;
    /// `!`
    pub const BANG: u8 = 33;
    /// `>`
    pub const PIPE: u8 = 62;
    /// `[`
    pub const L_SQ_BRACKET: u8 = 91;
    /// `\`
    pub const ESCAPE: u8 = 92;
    /// `]`
    pub const R_SQ_BRACKET: u8 = 93;
}

pub struct Config {
    socket: TcpListener,
    server_config: Arc<rustls::ServerConfig>,
    con_id: usize,
    storage: Storage,
    extensions: Extensions,
}
impl Config {
    pub fn on_port(port: u16) -> Self {
        Config {
            socket: TcpListener::bind(net::SocketAddr::new(
                net::IpAddr::V4(net::Ipv4Addr::new(0, 0, 0, 0)),
                port,
            ))
            .expect("Failed to bind to port"),
            server_config: Arc::new(
                tls_server_config::get_server_config("cert.pem", "privkey.pem")
                    .expect("Failed to read certificate"),
            ),
            con_id: RESERVED_TOKENS,
            storage: Storage::new(),
            extensions: Extensions::new(),
        }
    }
    pub fn with_config_on_port(config: rustls::ServerConfig, port: u16) -> Self {
        Config {
            socket: TcpListener::bind(net::SocketAddr::new(
                net::IpAddr::V4(net::Ipv4Addr::new(0, 0, 0, 0)),
                port,
            ))
            .expect("Failed to bind to port"),
            server_config: Arc::new(config),
            con_id: RESERVED_TOKENS,
            storage: Storage::new(),
            extensions: Extensions::new(),
        }
    }
    pub fn with_bindings(bindings: FunctionBindings, port: u16) -> Self {
        Config {
            socket: TcpListener::bind(net::SocketAddr::new(
                net::IpAddr::V4(net::Ipv4Addr::new(0, 0, 0, 0)),
                port,
            ))
            .expect("Failed to bind to port"),
            server_config: Arc::new(
                tls_server_config::get_server_config("cert.pem", "privkey.pem")
                    .expect("Failed to read certificate"),
            ),
            con_id: RESERVED_TOKENS,
            storage: Storage::from_bindings(Arc::new(bindings)),
            extensions: Extensions::new(),
        }
    }
    pub fn new(config: rustls::ServerConfig, bindings: FunctionBindings, port: u16) -> Self {
        Config {
            socket: TcpListener::bind(net::SocketAddr::new(
                net::IpAddr::V4(net::Ipv4Addr::new(0, 0, 0, 0)),
                port,
            ))
            .expect("Failed to bind to port"),
            server_config: Arc::new(config),
            con_id: RESERVED_TOKENS,
            storage: Storage::from_bindings(Arc::new(bindings)),
            extensions: Extensions::new(),
        }
    }

    /// Clones the Storage of this config, returning an owned reference-counted struct containing all caches and bindings
    #[inline]
    pub fn clone_storage(&self) -> Storage {
        Storage::clone(&self.storage)
    }
    /// Clones this configs inner config, returning a reference counted rustls ServerConfig
    #[inline]
    pub fn clone_inner(&self) -> Arc<rustls::ServerConfig> {
        Arc::clone(&self.server_config)
    }

    pub fn add_extension(&mut self, ext: BoundExtension) {
        self.extensions.add_extension(ext);
    }
    pub fn external_extension<F: Fn() -> BoundExtension>(&mut self, external_extension: F) {
        self.extensions.add_extension(external_extension());
    }

    /// Runs a server from the config on a new thread, not blocking the current thread.
    ///
    /// Use a loop to capture the main thread.
    ///
    /// # Examples
    /// ```no_run
    /// use arktis::Config;
    /// use std::io::{stdin, BufRead};
    /// use std::thread;
    ///
    /// let server = Config::on_port(443);
    /// let mut storage = server.clone_storage();
    ///
    /// thread::spawn(move || server.run());
    ///
    /// for line in stdin().lock().lines() {
    ///     if let Ok(line) = line {
    ///         let mut words = line.split(" ");
    ///         if let Some(command) = words.next() {
    ///             match command {
    ///                 "cfc" => match storage.try_fs() {
    ///                      Some(mut lock) => {
    ///                          lock.clear();
    ///                          println!("Cleared file system cache!");
    ///                      }
    ///                      None => println!("File system cache in use by server!"),
    ///                  },
    ///                  "crc" => match storage.try_response() {
    ///                      Some(mut lock) => {
    ///                          lock.clear();
    ///                          println!("Cleared response cache!");
    ///                      }
    ///                      None => println!("Response cache in use by server!"),
    ///                  },
    ///                 _ => {
    ///                     eprintln!("Unknown command!");
    ///                 }
    ///             }
    ///         }
    ///     };
    /// };
    ///
    /// ```
    pub fn run(mut self) {
        let mut poll = mio::Poll::new().expect("Failed to create a poll instance");
        let mut events = mio::Events::with_capacity(1024);
        poll.registry()
            .register(&mut self.socket, HTTPS_SERVER, mio::Interest::READABLE)
            .expect("Failed to register HTTPS server");

        let mut thread_handler = threading::HandlerPool::new(
            self.clone_inner(),
            self.clone_storage(),
            Extensions::clone(&self.extensions),
            poll.registry(),
        );

        loop {
            poll.poll(&mut events, None).expect("Failed to poll!");

            for event in events.iter() {
                match event.token() {
                    HTTPS_SERVER => {
                        self.accept(&mut thread_handler)
                            .expect("Failed to accept message!");
                    }
                    _ => {
                        let time = std::time::Instant::now();
                        thread_handler.handle(connection::MioEvent::from_event(event), time);
                    }
                }
            }
        }
    }
    #[inline]
    fn next_id(&mut self) -> usize {
        self.con_id = match self.con_id.checked_add(1) {
            Some(id) => id,
            None => RESERVED_TOKENS,
        };
        self.con_id
    }

    pub fn accept(&mut self, handler: &mut threading::HandlerPool) -> Result<(), std::io::Error> {
        loop {
            match self.socket.accept() {
                Ok((socket, addr)) => {
                    let token = mio::Token(self.next_id());
                    handler.accept(socket, addr, token);
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) => {
                    eprintln!("Encountered error while accepting connection. {:?}", err);
                    return Err(err);
                }
            }
        }
    }
}

pub struct Storage {
    fs: FsCache,
    response: ResponseCache,
    template: TemplateCache,
    bindings: Bindings,
}
impl Storage {
    pub fn new() -> Self {
        use cache::Cache;
        Storage {
            fs: Arc::new(Mutex::new(Cache::with_max_size(65536))),
            response: Arc::new(Mutex::new(Cache::new())),
            template: Arc::new(Mutex::new(Cache::with_max(128))),
            bindings: Arc::new(FunctionBindings::new()),
        }
    }
    pub fn from_caches(fs: FsCache, response: ResponseCache, template: TemplateCache) -> Self {
        Storage {
            fs,
            response,
            template,
            bindings: Arc::new(FunctionBindings::new()),
        }
    }
    pub fn from_bindings(bindings: Bindings) -> Self {
        use cache::Cache;
        Storage {
            fs: Arc::new(Mutex::new(Cache::with_max_size(65536))),
            response: Arc::new(Mutex::new(Cache::new())),
            template: Arc::new(Mutex::new(Cache::with_max(128))),
            bindings,
        }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.fs.lock().unwrap().clear();
        self.response.lock().unwrap().clear();
        self.template.lock().unwrap().clear();
    }

    /// Tries to get the lock of file cache.
    ///
    /// Always remember to handle the case if the lock isn't acquired; just don't return None!
    #[inline]
    pub fn try_fs(&mut self) -> Option<sync::MutexGuard<'_, FsCacheInner>> {
        #[cfg(feature = "no-fs-cache")]
        return None;
        #[cfg(not(feature = "no-fs-cache"))]
        match self.fs.try_lock() {
            Ok(lock) => Some(lock),
            Err(ref err) => match err {
                sync::TryLockError::WouldBlock => None,
                sync::TryLockError::Poisoned(..) => panic!("Lock is poisoned!"),
            },
        }
    }
    #[inline]
    pub fn get_fs(&mut self) -> &mut FsCache {
        &mut self.fs
    }
    /// Tries to get the lock of response cache.
    ///
    /// Always remember to handle the case if the lock isn't acquired; just don't return None!
    #[inline]
    pub fn try_response(&mut self) -> Option<sync::MutexGuard<'_, ResponseCacheInner>> {
        #[cfg(feature = "no-response-cache")]
        return None;
        #[cfg(not(feature = "no-response-cache"))]
        match self.response.try_lock() {
            Ok(lock) => Some(lock),
            Err(ref err) => match err {
                sync::TryLockError::WouldBlock => None,
                sync::TryLockError::Poisoned(..) => panic!("Lock is poisoned!"),
            },
        }
    }
    /// Gets the lock of response cache.
    #[inline]
    pub fn response_blocking(&mut self) -> Option<sync::MutexGuard<'_, ResponseCacheInner>> {
        #[cfg(feature = "no-response-cache")]
        return None;
        #[cfg(not(feature = "no-response-cache"))]
        match self.response.lock() {
            Ok(lock) => Some(lock),
            Err(..) => panic!("Lock is poisoned!"),
        }
    }
    /// Tries to get the lock of template cache.
    ///
    /// Always remember to handle the case if the lock isn't acquired; just don't return None!
    #[inline]
    pub fn try_template(&mut self) -> Option<sync::MutexGuard<'_, TemplateCacheInner>> {
        #[cfg(feature = "no-template-cache")]
        return None;
        #[cfg(not(feature = "no-template-cache"))]
        match self.template.try_lock() {
            Ok(lock) => Some(lock),
            Err(ref err) => match err {
                sync::TryLockError::WouldBlock => None,
                sync::TryLockError::Poisoned(..) => panic!("Lock is poisoned!"),
            },
        }
    }
    #[inline]
    pub fn get_bindings(&self) -> &Bindings {
        &self.bindings
    }
}
impl Clone for Storage {
    fn clone(&self) -> Self {
        Storage {
            fs: Arc::clone(&self.fs),
            response: Arc::clone(&self.response),
            template: Arc::clone(&self.template),
            bindings: Arc::clone(&self.bindings),
        }
    }
}

pub enum ParseCachedErr {
    StringEmpty,
    UndefinedKeyword,
    ContainsSpace,
    FailedToParse,
}
#[derive(Debug, Ord, PartialOrd, Eq, PartialEq)]
pub enum Cached {
    Dynamic,
    Changing,
    PerQuery,
    Static,
}
impl Cached {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        std::str::from_utf8(bytes).ok().and_then(|s| s.parse().ok())
    }

    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            Cached::Dynamic => b"Cache-Control: no-store\r\n",
            Cached::Changing => b"Cache-Control: max-age=120\r\n",
            Cached::Static | Cached::PerQuery => {
                b"Cache-Control: public, max-age=604800, immutable\r\n"
            }
        }
    }

    pub fn do_internal_cache(&self) -> bool {
        match self {
            Self::Dynamic | Self::Changing => false,
            Self::Static | Self::PerQuery => true,
        }
    }
    pub fn query_matters(&self) -> bool {
        match self {
            Self::Dynamic | Self::PerQuery => true,
            Self::Static | Self::Changing => false,
        }
    }
    pub fn cached_without_query(&self) -> bool {
        match self {
            Self::Dynamic | Self::PerQuery | Self::Changing => false,
            Self::Static => true,
        }
    }
}
impl std::str::FromStr for Cached {
    type Err = ParseCachedErr;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.contains(' ') {
            Err(Self::Err::ContainsSpace)
        } else {
            match s.to_ascii_lowercase().as_str() {
                "false" | "no-cache" | "dynamic" => Ok(Self::Dynamic),
                "changing" | "may-change" => Ok(Self::Changing),
                "per-query" | "query" => Ok(Self::PerQuery),
                "true" | "static" | "immutable" => Ok(Self::Static),
                "" => Err(Self::Err::StringEmpty),
                _ => Err(Self::Err::UndefinedKeyword),
            }
        }
    }
}

/// The main request processing function.
///
/// First checks if something's in cache, then write it to the socket and return.
///
/// Then, check if a binding is available. If one is, give it a `Vec` to populate. Wrap that `Vec` in a `ByteResponse` to get separation between body and head.
/// If not, get from the FS instead, and wrap in `Arc` inside a `ByteResponse`. Sets appropriate content type and cache settings.
///
/// Then matches content type to get a `str`.
///
/// Checks extension in body of `ByteResponse`.
fn process_request<W: Write>(
    socket: &mut W,
    request: http::Request<&[u8]>,
    raw_request: &[u8],
    close: &connection::ConnectionHeader,
    storage: &mut Storage,
    extensions: &mut ExtensionMap,
) -> Result<(), io::Error> {
    let is_get = match request.method() {
        &http::Method::GET | &http::Method::HEAD => true,
        _ => false,
    };
    let mut allowed_method = is_get;

    // println!("Got request: {:?}", &request);
    if is_get {
        // Load from cache
        // Try get response cache lock
        if let Some(lock) = storage.response_blocking() {
            // If response is in cache
            if let Some(response) = lock.resolve(request.uri(), request.headers()) {
                // println!("Got cache! {}", request.uri());
                return response.write_as_method(socket, request.method());
            }
        }
    }

    // Get from function or cache, to enable processing (extensions) from functions!
    let path = match parse::convert_uri(request.uri()) {
        Ok(path) => path,
        Err(()) => {
            &default_error(403, close, Some(storage.get_fs())).write_all(socket)?;
            return Ok(());
        }
    };

    // Extensions need body and cache setting to be mutable; to replace it.
    // Used to bypass immutable/mutable rule. It is safe because the binding reference isn't affected by changing the cache.
    let cache: *mut Storage = storage;
    let (mut byte_response, mut content_type, mut cached) =
        match storage.get_bindings().get_binding(request.uri().path()) {
            // We've got an function, call it and return body and result!
            Some(callback) => {
                let mut response = Vec::with_capacity(2048);
                let (content_type, cache) =
                    callback(&mut response, &request, unsafe { (*cache).get_fs() });

                allowed_method = true;
                // Check if callback contains headers. Change to response struct in future!
                if &response[..5] == b"HTTP/" {
                    (ByteResponse::with_header(response), content_type, cache)
                } else {
                    (ByteResponse::without_header(response), content_type, cache)
                }
            }
            // No function, try read from FS cache.
            None => {
                // Body
                let body = match read_file(&path, storage.get_fs()) {
                    Some(response) => ByteResponse::without_header_shared(response),
                    None => default_error(404, close, Some(storage.get_fs())),
                };
                // Content mime type
                (body, ContentType::AutoOrDownload, Cached::Static)
            }
        };

    // Apply extensions
    {
        pub use extensions::FileType::*;
        pub use extensions::KnownExtension::*;

        {
            // Search through extension map!
            let (extension_args, content_start) =
                extensions::extension_args(byte_response.get_body());
            let name = extension_args
                .get(0)
                .and_then(|arg| std::str::from_utf8(arg).ok());
            let file_extension = path.extension().and_then(|path| path.to_str());
            match extensions.get(name, file_extension) {
                Some(extension) => {
                    let args = extension_args
                        .iter()
                        .filter_map(|bytes| {
                            std::str::from_utf8(bytes)
                                .ok()
                                .map(|valid_str| valid_str.to_owned())
                        })
                        .collect();
                    unsafe {
                        extension.run(RequestData {
                            response: &mut byte_response,
                            content_start,
                            cached: &mut cached,
                            args,
                            storage,
                            request: &request,
                            raw_request,
                            path: &path,
                            content_type: &mut content_type,
                        })
                    }
                }
                // Do nothing
                None if allowed_method => {}
                _ => {
                    byte_response = default_error(405, close, Some(storage.get_fs()));
                }
            };
        }

        // Needs to be removed soon.
        // Remove or optimize
        match extensions::identify(
            byte_response.get_body(),
            path.extension().and_then(|path| path.to_str()),
        ) {
            // An extension is identified, handle it!
            // NEEDS TO COVER ALL POSSIBILITIES FOR LAST `_ =>` TO MAKE SENSE!
            DefinedExtension(extension, content_start, template_args) => match extension {
                #[cfg(feature = "templates")]
                Template if allowed_method => {
                    byte_response = ByteResponse::without_header(extensions::template(
                        &template_args[..],
                        byte_response.body_from(content_start),
                        storage,
                    ));
                }
                SetCache if allowed_method => {
                    if let Some(cache) =
                        template_args.get(1).and_then(|arg| Cached::from_bytes(arg))
                    {
                        cached = cache;
                    }
                }
                // If method didn't match, return 405 err!
                _ => {
                    byte_response = default_error(405, close, Some(&mut storage.fs));
                }
            },
            // Remove the extension definition.
            UnknownExtension(content_start, _) if allowed_method => {
                byte_response =
                    ByteResponse::without_header(byte_response.body_from(content_start).to_vec());
            }
            // Do nothing!
            Raw if allowed_method => {}
            // If method didn't match, return 405 err!
            _ => {
                byte_response = default_error(405, close, Some(&mut storage.fs));
            }
        };
    }

    if cached.cached_without_query() {
        let bytes = request.uri().path().as_bytes().to_vec(); // ToDo: Remove cloning of slice!
        if let Ok(uri) = http::Uri::from_maybe_shared(bytes) {
            if let Some(lock) = storage.response_blocking() {
                if let Some(response) = lock.resolve(&uri, request.headers()) {
                    return response.write_as_method(socket, request.method());
                };
            }
        }
    }

    let content_str = content_type.as_str(path);
    // The response MUST contain all vary headers, else it won't be cached!
    let vary: Vec<&str> = vec![/* "Content-Type", */ "Accept-Encoding"];

    let compression = match request
        .headers()
        .get("Accept-Encoding")
        .and_then(|header| header.to_str().ok())
    {
        Some(header) => {
            let (algorithm, identity_forbidden) = compression_from_header(header);
            // Filter content types for compressed formats
            if (content_str.starts_with("application")
                && !content_str.contains("xml")
                && !content_str.contains("json")
                && content_str != "application/pdf"
                && content_str != "application/javascript"
                && content_str != "application/graphql")
                || content_str.starts_with("image")
                || content_str.starts_with("audio")
                || content_str.starts_with("video")
            {
                if identity_forbidden {
                    byte_response = default_error(406, &close, Some(&mut storage.fs));
                    algorithm
                } else {
                    CompressionAlgorithm::Identity
                }
            } else {
                algorithm
            }
        }
        None => CompressionAlgorithm::Identity,
    };

    let response = match byte_response {
        ByteResponse::Merged(_, _, partial_header) if partial_header => {
            let partial_head = byte_response.get_head().unwrap();
            let mut head = Vec::with_capacity(2048);
            if !partial_head.starts_with(b"HTTP") {
                head.extend(b"HTTP/1.1 200 OK\r\n");
            }
            // Adding partial head
            head.extend_from_slice(partial_head);
            // Remove last CRLF if, header doesn't end here!
            if head.ends_with(&[CR, LF]) {
                head.truncate(head.len() - 2);
            }
            // Parse the present headers
            let present_headers = parse::parse_only_headers(partial_head);
            let compress = !present_headers.contains_key(CONTENT_ENCODING);
            let varies = present_headers.contains_key(VARY);
            let body = if compress && !varies {
                Compressors::compress(byte_response.get_body(), &compression)
            } else {
                byte_response.into_body()
            };

            use http::header::*;

            if !present_headers.contains_key(CONNECTION) {
                head.extend(b"Connection: ");
                head.extend(close.as_bytes());
                head.extend(LINE_ENDING);
            }
            if compress && !varies {
                // Compression
                head.extend(b"Content-Encoding: ");
                head.extend(compression.as_bytes());
                head.extend(LINE_ENDING);
            }
            if !present_headers.contains_key(CONTENT_LENGTH) {
                // Length
                head.extend(b"Content-Length: ");
                head.extend(format!("{}\r\n", body.len()).as_bytes());
            }

            if !present_headers.contains_key(CONTENT_TYPE) {
                head.extend(b"Content-Type: ");
                head.extend(content_str.as_bytes());
                head.extend(LINE_ENDING);
            }
            if !present_headers.contains_key(CACHE_CONTROL) {
                // Cache header!
                head.extend(cached.as_bytes());
            }

            if !varies && !vary.is_empty() {
                head.extend(b"Vary: ");
                let mut iter = vary.iter();
                head.extend(iter.next().unwrap().as_bytes());

                for vary in iter {
                    head.extend(b", ");
                    head.extend(vary.as_bytes());
                }
                head.extend(LINE_ENDING);
            }

            // Add server signature
            head.extend(SERVER_HEADER);
            // Close header
            head.extend(LINE_ENDING);

            // Return byte response
            ByteResponse::Both(head, body)
        }
        ByteResponse::Body(_) | ByteResponse::BorrowedBody(_) => {
            let mut response = Vec::with_capacity(4096);
            response.extend(b"HTTP/1.1 200 OK\r\n");
            response.extend(b"Connection: ");
            response.extend(close.as_bytes());
            response.extend(LINE_ENDING);
            // Compression
            response.extend(b"Content-Encoding: ");
            response.extend(compression.as_bytes());
            response.extend(LINE_ENDING);
            let body = Compressors::compress(byte_response.get_body(), &compression);
            // Length
            response.extend(b"Content-Length: ");
            response.extend(format!("{}\r\n", body.len()).as_bytes());

            response.extend(b"Content-Type: ");
            response.extend(content_str.as_bytes());
            response.extend(LINE_ENDING);
            // Cache header!
            response.extend(cached.as_bytes());

            if !vary.is_empty() {
                response.extend(b"Vary: ");
                let mut iter = vary.iter();
                // Can unwrap, since it isn't empty!
                response.extend(iter.next().unwrap().as_bytes());

                for vary in iter {
                    response.extend(b", ");
                    response.extend(vary.as_bytes());
                }
                response.extend(LINE_ENDING);
            }

            response.extend(SERVER_HEADER);
            // Close header
            response.extend(LINE_ENDING);

            // Return byte response
            ByteResponse::Both(response, body)
        }
        // Headers handled! Taking for granted user handled HEAD method.
        _ => byte_response,
    };

    // Write to socket!
    response.write_as_method(socket, request.method())?;

    if is_get && cached.do_internal_cache() {
        if let Some(mut lock) = storage.response_blocking() {
            let uri = request.into_parts().0.uri;
            let uri = if !cached.query_matters() {
                let bytes = uri.path().as_bytes().to_vec(); // ToDo: Remove cloning of slice!
                if let Ok(uri) = http::Uri::from_maybe_shared(bytes) {
                    uri
                } else {
                    uri
                }
            } else {
                uri
            };
            println!("Caching uri {}", &uri);
            match vary.is_empty() {
                false => {
                    let headers = {
                        let headers = parse::parse_only_headers(response.get_head().unwrap());
                        let mut is_ok = true;
                        let mut buffer = Vec::with_capacity(vary.len());
                        for vary_header in vary.iter() {
                            let header = match *vary_header {
                                "Accept-Encoding" => "Content-Encoding",
                                _ => *vary_header,
                            };
                            match headers.get(header) {
                                Some(header) => {
                                    buffer.push(header.clone()) // ToDo: Remove in future!
                                }
                                None => {
                                    is_ok = false;
                                    break;
                                }
                            }
                        }
                        match is_ok {
                            true => Some(buffer),
                            false => None,
                        }
                    };
                    match headers {
                        Some(headers) => {
                            let _ = lock.add_variant(uri, response, headers, &vary[..]);
                        }
                        None => eprintln!("Vary header not present in response!"),
                    }
                }
                true => {
                    let _ = lock.cache(uri, Arc::new(cache::CacheType::with_data(response)));
                }
            }
        }
    }
    Ok(())
}

fn default_error(
    code: u16,
    close: &connection::ConnectionHeader,
    cache: Option<&mut FsCache>,
) -> ByteResponse {
    let mut buffer = Vec::with_capacity(512);
    buffer.extend(b"HTTP/1.1 ");
    buffer.extend(
        format!(
            "{}\r\n",
            http::StatusCode::from_u16(code).unwrap_or(http::StatusCode::from_u16(500).unwrap())
        )
        .as_bytes(),
    );
    buffer.extend(
        &b"Content-Type: text/html\r\n\
        Connection: "[..],
    );
    if close.close() {
        buffer.extend(b"Close\r\n");
    } else {
        buffer.extend(b"Keep-Alive\r\n");
    }
    buffer.extend(b"Content-Encoding: identity\r\n");

    let body = match cache
        .and_then(|cache| read_file(&PathBuf::from(format!("{}.html", code)), cache))
    {
        Some(file) => {
            buffer.extend(b"Content-Length: ");
            buffer.extend(format!("{}\r\n\r\n", file.len()).as_bytes());
            // buffer.extend(file.get_body());
            (*file).clone()
        }
        None => {
            let mut body = Vec::with_capacity(1024);
            // let error = get_default(code);
            match code {
                _ => {
                    // Get code and reason!
                    let status = http::StatusCode::from_u16(code).ok();
                    let write_code = |body: &mut Vec<_>| match status {
                        #[inline]
                        Some(status) => body.extend(status.as_str().as_bytes()),
                        None => body.extend(format!("{}", code).as_bytes()),
                    };
                    let reason = status.and_then(|status| status.canonical_reason());

                    body.extend(b"<html><head><title>");
                    // Code and reason
                    write_code(&mut body);
                    body.extend(b" ");
                    if let Some(reason) = reason {
                        body.extend(reason.as_bytes());
                    }

                    body.extend(&b"</title></head><body><center><h1>"[..]);
                    // Code and reason
                    write_code(&mut body);
                    body.extend(b" ");
                    if let Some(reason) = reason {
                        body.extend(reason.as_bytes());
                    }
                    body.extend(&b"</h1><hr>An unexpected error occurred. <a href='/'>Return home</a>?</center></body></html>"[..]);
                }
            }

            buffer.extend(b"Content-Length: ");
            buffer.extend(format!("{}\r\n\r\n", body.len()).as_bytes());
            // buffer.append(&mut body);
            body
        }
    };

    ByteResponse::Both(buffer, body)
}

/// Writes a generic error to `buffer`.
/// For the version using the file system to deliver error messages, use `write_error`.
///
/// Returns (`text/html`, `Cached::Static`) to feed to binding closure.
/// If you don't want it to cache, construct a custom return value.
///
/// # Examples
/// ```
/// use arktis::{FunctionBindings, write_generic_error};
///
/// let mut bindings = FunctionBindings::new();
///
/// bindings.bind_page("/throw_500", |mut buffer, _, _| {
///   write_generic_error(&mut buffer, 500)
/// });
/// ```
pub fn write_generic_error(buffer: &mut Vec<u8>, code: u16) -> (ContentType, Cached) {
    default_error(code, &connection::ConnectionHeader::KeepAlive, None)
        .write_all(buffer)
        .expect("Failed to write to vec!");
    (ContentType::Html, Cached::Dynamic)
}
/// Writes a error to `buffer`.
/// For the version not using the file system, but generic hard-coded errors, use `write_generic_error`.
///
/// Returns (`text/html`, `Cached::Static`) to feed to binding closure.
/// If you don't want it to cache, construct a custom return value.
///
/// # Examples
/// ```
/// use arktis::{FunctionBindings, write_error};
///
/// let mut bindings = FunctionBindings::new();
///
/// bindings.bind_page("/throw_500", |mut buffer, _, storage| {
///   write_error(&mut buffer, 500, storage)
/// });
/// ```
pub fn write_error(buffer: &mut Vec<u8>, code: u16, cache: &mut FsCache) -> (ContentType, Cached) {
    default_error(code, &connection::ConnectionHeader::KeepAlive, Some(cache))
        .write_all(buffer)
        .expect("Failed to write to vec!");
    (ContentType::Html, Cached::Dynamic)
}

fn read_file(path: &PathBuf, cache: &mut FsCache) -> Option<Arc<Vec<u8>>> {
    match cache.try_lock() {
        Ok(lock) => {
            if let Some(cached) = lock.get(path) {
                return Some(cached);
            }
        }
        Err(ref err) => match err {
            std::sync::TryLockError::Poisoned(..) => {
                panic!("File System cache is poisoned!");
            }
            std::sync::TryLockError::WouldBlock => {}
        },
    }

    match File::open(path) {
        Ok(mut file) => {
            let mut buffer = Vec::with_capacity(4096);
            match file.read_to_end(&mut buffer) {
                Ok(..) => {
                    let buffer = Arc::new(buffer);
                    match cache.try_lock() {
                        Ok(mut lock) => match lock.cache(path.clone(), buffer) {
                            Err(failed) => Some(failed),
                            Ok(()) => Some(lock.get(path).unwrap()),
                        },
                        Err(ref err) => match err {
                            std::sync::TryLockError::Poisoned(..) => {
                                panic!("File System cache is poisoned!");
                            }
                            std::sync::TryLockError::WouldBlock => Some(buffer),
                        },
                    }
                }
                Err(..) => None,
            }
        }
        Err(..) => None,
    }
}

#[allow(dead_code)]
enum Compressors {
    Raw(Vec<u8>),
    #[cfg(feature = "br")]
    Brotli(brotli::CompressorWriter<Vec<u8>>),
    #[cfg(feature = "gzip")]
    Gzip(flate2::write::GzEncoder<Vec<u8>>),
}
#[allow(dead_code)]
impl Compressors {
    #[inline]
    pub fn new(vec: Vec<u8>, compressor: &CompressionAlgorithm) -> Self {
        match compressor {
            #[cfg(feature = "br")]
            CompressionAlgorithm::Brotli => Self::brotli(vec),
            #[cfg(feature = "gzip")]
            CompressionAlgorithm::Gzip => Self::gzip(vec),
            CompressionAlgorithm::Identity => Self::raw(vec),
        }
    }
    #[inline]
    pub fn raw(vec: Vec<u8>) -> Self {
        Self::Raw(vec)
    }
    #[inline]
    #[cfg(feature = "br")]
    pub fn brotli(vec: Vec<u8>) -> Self {
        Self::Brotli(brotli::CompressorWriter::new(vec, 4096, 8, 21))
    }
    #[inline]
    #[cfg(feature = "gzip")]
    pub fn gzip(vec: Vec<u8>) -> Self {
        Self::Gzip(flate2::write::GzEncoder::new(
            vec,
            flate2::Compression::fast(),
        ))
    }

    /// Very small footprint.
    ///
    /// On identity compressing, only takes allocation and copying time; only few micro seconds.
    pub fn compress(bytes: &[u8], compressor: &CompressionAlgorithm) -> Vec<u8> {
        match compressor {
            CompressionAlgorithm::Identity => bytes.to_vec(),
            CompressionAlgorithm::Brotli => {
                let buffer = Vec::with_capacity(bytes.len() / 3 + 128);
                let mut c = brotli::CompressorWriter::new(buffer, 4096, 8, 21);
                c.write(bytes).expect("Failed to compress using Brotli!");
                c.flush().expect("Failed to compress using Brotli!");
                let mut buffer = c.into_inner();
                buffer.shrink_to_fit();
                buffer
            }
            CompressionAlgorithm::Gzip => {
                let buffer = Vec::with_capacity(bytes.len() / 3 + 128);
                let mut c = flate2::write::GzEncoder::new(buffer, flate2::Compression::fast());
                c.write(bytes).expect("Failed to compress using gzip!");
                let mut buffer = c.finish().expect("Failed to compress using gzip!");
                buffer.shrink_to_fit();
                buffer
            }
        }
    }

    #[inline]
    pub fn write(&mut self, bytes: &[u8]) {
        match self {
            Self::Raw(buffer) => {
                buffer.extend(bytes);
            }
            #[cfg(feature = "br")]
            Self::Brotli(compressor) => {
                if let Err(err) = compressor.write_all(bytes) {
                    eprintln!("Error compressing: {}", err);
                };
            }
            #[cfg(feature = "gzip")]
            Self::Gzip(compressor) => {
                if let Err(err) = compressor.write_all(bytes) {
                    eprintln!("Error compressing: {}", err);
                };
            }
        }
    }
    #[inline]
    pub fn finish(self) -> Vec<u8> {
        match self {
            Self::Raw(buffer) => buffer,
            #[cfg(feature = "br")]
            Self::Brotli(compressor) => compressor.into_inner(),
            #[cfg(feature = "gzip")]
            Self::Gzip(compressor) => compressor.finish().unwrap(),
        }
    }
}

/// Types of encoding to use.
///
/// Does not include DEFLATE because of bad support
enum CompressionAlgorithm {
    #[cfg(feature = "br")]
    Brotli,
    #[cfg(feature = "gzip")]
    Gzip,
    // Deflate,
    Identity,
}
impl CompressionAlgorithm {
    pub fn as_bytes(&self) -> &'static [u8] {
        match self {
            CompressionAlgorithm::Identity => b"identity",
            #[cfg(feature = "br")]
            CompressionAlgorithm::Brotli => b"br",
            #[cfg(feature = "gzip")]
            CompressionAlgorithm::Gzip => b"gzip",
        }
    }
}
fn compression_from_header(header: &str) -> (CompressionAlgorithm, bool) {
    let header = header.to_ascii_lowercase();
    let mut options = parse::format_list_header(&header);

    options.sort_by(|a, b| b.quality.partial_cmp(&a.quality).unwrap());

    let identity = options.iter().position(|option| option == "identity");
    let identity_forbidden = if let Some(identity) = identity {
        options.get(identity).unwrap().quality == 0.0
    } else {
        false
    };

    // println!("Options: {:?}", options);

    // If Brotli enabled, prioritize it if quality == 1
    #[cfg(feature = "br")]
    if options.is_empty()
        || options.iter().any(|option| {
            option
                == &parse::ValueQualitySet {
                    value: "br",
                    quality: 1.0,
                }
        })
    {
        return (CompressionAlgorithm::Brotli, identity_forbidden);
    }
    match options[0].value {
        #[cfg(feature = "gzip")]
        "gzip" => (CompressionAlgorithm::Gzip, identity_forbidden),
        #[cfg(feature = "br")]
        "br" => (CompressionAlgorithm::Brotli, identity_forbidden),
        _ => (CompressionAlgorithm::Identity, identity_forbidden),
    }
}

pub mod cache {
    use super::*;
    use http::Uri;
    use std::collections::HashMap;
    use std::{borrow::Borrow, hash::Hash};

    /// A response in byte form to query head or only body. Can be used when a if a buffer contains HTTP headers is unknown.
    ///
    /// Variants `Body` and `BorrowedBody` doesn't contain a head, a head in `Merged` is optional.
    pub enum ByteResponse {
        Merged(Vec<u8>, usize, bool),
        Both(Vec<u8>, Vec<u8>),
        Body(Vec<u8>),
        BorrowedBody(Arc<Vec<u8>>),
    }
    impl ByteResponse {
        #[inline]
        pub fn with_header(bytes: Vec<u8>) -> Self {
            let start = Self::get_start(&bytes[..]);
            Self::Merged(bytes, start, false)
        }
        #[inline]
        pub fn with_partial_header(bytes: Vec<u8>) -> Self {
            let start = Self::get_start(&bytes[..]);
            Self::Merged(bytes, start, true)
        }
        #[inline]
        pub fn without_header(body: Vec<u8>) -> Self {
            Self::Body(body)
        }
        #[inline]
        pub fn without_header_shared(shared_body: Arc<Vec<u8>>) -> Self {
            Self::BorrowedBody(shared_body)
        }

        fn get_start(bytes: &[u8]) -> usize {
            let mut newlines_in_row = 0;
            for (position, byte) in bytes.iter().enumerate() {
                match *byte {
                    LF | CR => newlines_in_row += 1,
                    _ => newlines_in_row = 0,
                }
                if newlines_in_row == 4 {
                    return position + 1;
                }
            }
            0
        }
        #[inline]
        pub fn len(&self) -> usize {
            match self {
                Self::Merged(vec, _, _) => vec.len(),
                Self::Both(head, body) => head.len() + body.len(),
                Self::Body(body) => body.len(),
                Self::BorrowedBody(borrow) => borrow.len(),
            }
        }

        #[inline]
        pub fn write_all<W: Write>(&self, writer: &mut W) -> io::Result<()> {
            match self {
                Self::Merged(vec, _, _) => writer.write_all(&vec[..]),
                Self::Both(head, body) => {
                    writer.write_all(&head[..])?;
                    writer.write_all(&body[..])
                }
                Self::Body(body) => writer.write_all(&body[..]),
                Self::BorrowedBody(borrow) => writer.write_all(&borrow[..]),
            }
        }
        pub fn write_as_method<W: Write>(
            &self,
            writer: &mut W,
            method: &http::Method,
        ) -> io::Result<()> {
            match method {
                &http::Method::HEAD => {
                    if let Some(head) = self.get_head() {
                        writer.write_all(head)
                    } else {
                        Ok(())
                    }
                }
                _ => match self {
                    Self::Merged(vec, _, _) => writer.write_all(&vec[..]),
                    Self::Both(head, body) => {
                        writer.write_all(&head[..])?;
                        writer.write_all(&body[..])
                    }
                    Self::Body(body) => writer.write_all(&body[..]),
                    Self::BorrowedBody(borrow) => writer.write_all(&borrow[..]),
                },
            }
        }

        #[inline]
        pub fn get_head(&self) -> Option<&[u8]> {
            match self {
                Self::Merged(vec, start, _) if *start > 0 => Some(&vec[..*start]),
                Self::Both(head, _) => Some(&head[..]),
                _ => None,
            }
        }
        #[inline]
        pub fn into_head(self) -> Vec<u8> {
            match self {
                Self::Merged(mut vec, start, _) if start > 0 => {
                    vec.truncate(start);
                    vec
                }
                Self::Both(head, _) => head,
                _ => Vec::new(),
            }
        }
        #[inline]
        pub fn get_body(&self) -> &[u8] {
            match self {
                Self::Merged(vec, start, _) => &vec[*start..],
                Self::Both(_, body) => &body[..],
                Self::Body(body) => &body[..],
                Self::BorrowedBody(borrow) => &borrow[..],
            }
        }
        #[inline]
        pub fn into_body(self) -> Vec<u8> {
            match self {
                Self::Merged(mut vec, start, _) => {
                    let p = vec.as_mut_ptr();
                    let len = vec.len();
                    let cap = vec.capacity();

                    unsafe {
                        Vec::from_raw_parts(p.offset(start as isize), len - start, cap - start)
                    }
                }
                Self::Both(_, body) => body,
                Self::Body(body) => body,
                Self::BorrowedBody(borrowed) => (*borrowed).clone(),
            }
        }
        #[inline]
        pub fn body_from(&self, from: usize) -> &[u8] {
            &self.get_body()[from..]
        }
        #[inline]
        pub fn body_to(&self, to: usize) -> &[u8] {
            &self.get_body()[..to]
        }
    }
    impl std::fmt::Debug for ByteResponse {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
            match self {
                Self::Merged(_, starts_at, _) => {
                    write!(f, "ByteResponse::Merged, starts at {}", starts_at)
                }
                Self::Both(_, _) => write!(f, "ByteResponse::Both"),
                Self::Body(_) => write!(f, "ByteResponse::Body"),
                Self::BorrowedBody(_) => write!(f, "ByteResponse::BorrowedBody"),
            }
        }
    }
    pub struct VaryMaster {
        vary_headers: Vec<&'static str>,
        data: Mutex<Vec<(Vec<http::HeaderValue>, Arc<ByteResponse>)>>,
    }

    /// A enum to contain data about the cached data. Can either be `Data`, when no `Vary` header is present, or `Vary` if it must contain several values.
    pub enum CacheType {
        Data(Arc<ByteResponse>),
        Vary(VaryMaster),
    }
    impl CacheType {
        pub fn with_data(data: ByteResponse) -> Self {
            Self::Data(Arc::new(data))
        }
        pub fn vary(headers: Vec<&'static str>) -> Self {
            Self::Vary(VaryMaster {
                vary_headers: headers,
                data: Mutex::new(Vec::new()),
            })
        }
        pub fn vary_with_data(
            structure: Vec<&'static str>,
            data: Arc<ByteResponse>,
            headers: Vec<http::HeaderValue>,
        ) -> Self {
            Self::Vary(VaryMaster {
                vary_headers: structure,
                data: Mutex::new(vec![(headers, data)]),
            })
        }

        pub fn resolve(&self, headers: &http::HeaderMap) -> Option<Arc<ByteResponse>> {
            match self {
                Self::Data(data) => Some(Arc::clone(data)),
                Self::Vary(vary) => {
                    let mut results = Vec::new();
                    let mut iter = vary.vary_headers.iter().enumerate();

                    let all_data = vary.data.lock().unwrap();
                    {
                        let (position, name) = iter.next().unwrap();

                        let required_data = {
                            headers.get(*name);
                            Vec::<&str>::new()
                        };

                        // Match with all cached data!
                        for data in all_data.iter() {
                            // If nothing required
                            if required_data.is_empty() {
                                // Push!
                                results.push(data);
                            } else {
                                'match_supported: for supported_header in required_data.iter() {
                                    // If any header contains star or matches required!
                                    if data.0.get(position).unwrap() == supported_header
                                        || supported_header.starts_with('*')
                                    {
                                        results.push(data);
                                        break 'match_supported;
                                    }
                                }
                            }
                        }
                    }
                    for (position, header_to_compare) in iter {
                        results.retain(|&current| {
                            let required_data = {
                                headers.get(*header_to_compare);
                                Vec::<&str>::new()
                            };

                            if required_data.is_empty() {
                                // Keep!
                                return true;
                            } else {
                                for supported_header in required_data.iter() {
                                    // If any header contains star or matches required!
                                    if current.0.get(position).unwrap() == supported_header
                                        || supported_header.starts_with('*')
                                    {
                                        return true;
                                    }
                                }
                            }
                            false
                        })
                    }

                    results.get(0).map(|result| Arc::clone(&result.1))
                }
            }
        }

        pub fn add_variant(
            &self,
            response: Arc<ByteResponse>,
            headers: Vec<http::HeaderValue>,
            structure: &[&'static str],
        ) -> Result<(), Arc<ByteResponse>> {
            match self {
                // So data (header) structure is identical
                Self::Vary(vary) if structure == vary.vary_headers => {
                    let mut data = vary.data.lock().unwrap();
                    data.push((headers, response));
                    Ok(())
                }
                _ => Err(response),
            }
        }
    }
    impl<K: Clone + Hash + Eq> Cache<K, CacheType> {
        pub fn resolve<Q: ?Sized + Hash + Eq>(
            &self,
            key: &Q,
            headers: &http::HeaderMap,
        ) -> Option<Arc<ByteResponse>>
        where
            K: Borrow<Q>,
        {
            let data = self.get(key)?;
            data.resolve(headers)
        }
        pub fn add_variant(
            &mut self,
            key: K,
            response: ByteResponse,
            headers: Vec<http::HeaderValue>,
            structure: &[&'static str],
        ) -> Result<(), ()> {
            match self.get(&key) {
                Some(varied) => {
                    if response.size() > self.size_limit {
                        return Err(());
                    }
                    varied
                        .add_variant(Arc::new(response), headers, structure)
                        .or(Err(()))
                }
                None => self
                    .cache(
                        key,
                        Arc::new(CacheType::vary_with_data(
                            structure.to_vec(),
                            Arc::new(response),
                            headers,
                        )),
                    )
                    .or(Err(())),
            }
        }
    }

    pub mod types {
        use super::*;

        pub type FsCacheInner = Cache<PathBuf, Vec<u8>>;
        pub type FsCache = Arc<Mutex<FsCacheInner>>;
        pub type ResponseCacheInner = Cache<Uri, CacheType>;
        pub type ResponseCache = Arc<Mutex<ResponseCacheInner>>;
        pub type TemplateCacheInner = Cache<String, HashMap<Arc<String>, Arc<Vec<u8>>>>;
        pub type TemplateCache = Arc<Mutex<TemplateCacheInner>>;
        pub type Bindings = Arc<FunctionBindings>;
    }

    pub trait Size {
        fn size(&self) -> usize;
    }
    impl<T> Size for Vec<T> {
        fn size(&self) -> usize {
            self.len() * std::mem::size_of::<T>()
        }
    }
    impl<T> Size for dyn Borrow<Vec<T>> {
        fn size(&self) -> usize {
            self.borrow().len() * std::mem::size_of::<T>()
        }
    }
    impl<K, V> Size for HashMap<K, V> {
        fn size(&self) -> usize {
            self.len() * std::mem::size_of::<V>()
        }
    }
    impl<K, V> Size for dyn Borrow<HashMap<K, V>> {
        fn size(&self) -> usize {
            self.borrow().len() * std::mem::size_of::<V>()
        }
    }
    impl Size for ByteResponse {
        fn size(&self) -> usize {
            self.len()
        }
    }
    impl Size for CacheType {
        fn size(&self) -> usize {
            match self {
                Self::Vary(vary) => {
                    // for data in  {}
                    vary.data
                        .lock()
                        .unwrap()
                        .iter()
                        .fold(0, |acc, data| acc + data.1.size())
                }
                Self::Data(data) => data.size(),
            }
        }
    }

    pub struct Cache<K, V> {
        map: HashMap<K, Arc<V>>,
        max_items: usize,
        size_limit: usize,
    }
    #[allow(dead_code)]
    impl<K: Eq + Hash + Clone, V: Size> Cache<K, V> {
        #[inline]
        pub fn cache(&mut self, key: K, value: Arc<V>) -> Result<(), Arc<V>> {
            if value.size() > self.size_limit {
                return Err(value);
            }
            if self.map.len() >= self.max_items {
                // Reduce number of items!
                if let Some(last) = self.map.iter().next().map(|value| value.0.clone()) {
                    self.map.remove(&last);
                }
            }
            self.map.insert(key, value);
            Ok(())
        }
    }
    impl<K: Eq + Hash + Clone, V> Cache<K, V> {
        pub fn new() -> Self {
            Cache {
                map: HashMap::with_capacity(64),
                max_items: 1024,
                size_limit: 4194304, // 4MiB
            }
        }
        pub fn with_max(max_items: usize) -> Self {
            assert!(max_items > 1);
            Cache {
                map: HashMap::with_capacity(max_items / 16 + 1),
                max_items,
                size_limit: 4194304, // 4MiB
            }
        }
        pub fn with_max_size(max_size: usize) -> Self {
            assert!(max_size > 1024);
            Cache {
                map: HashMap::with_capacity(64),
                max_items: 1024,
                size_limit: max_size,
            }
        }
        pub fn with_max_and_size(max_items: usize, size_limit: usize) -> Self {
            assert!(max_items > 1);
            assert!(size_limit >= 1024);

            Cache {
                map: HashMap::with_capacity(max_items / 16 + 1),
                max_items,
                size_limit,
            }
        }
        #[inline]
        pub fn get<Q: ?Sized + Hash + Eq>(&self, key: &Q) -> Option<Arc<V>>
        where
            K: Borrow<Q>,
        {
            self.map.get(key).map(|value| Arc::clone(value))
        }
        #[inline]
        pub fn cached(&self, key: &K) -> bool {
            self.map.contains_key(key)
        }
        #[inline]
        pub fn remove(&mut self, key: &K) -> Option<Arc<V>> {
            self.map.remove(key)
        }
        #[inline]
        pub fn clear(&mut self) {
            self.map.clear()
        }
    }
}

pub mod connection {
    use super::*;
    use http::Version;
    use mio::{event::Event, Interest, Registry, Token};
    use rustls::{ServerSession, Session};

    #[derive(PartialEq, Debug)]
    pub enum ConnectionHeader {
        KeepAlive,
        Close,
    }
    impl ConnectionHeader {
        pub fn from_close(close: bool) -> Self {
            if close {
                Self::Close
            } else {
                Self::KeepAlive
            }
        }
        pub fn close(&self) -> bool {
            *self == Self::Close
        }
        pub fn as_bytes(&self) -> &'static [u8] {
            match self {
                ConnectionHeader::Close => b"close",
                ConnectionHeader::KeepAlive => b"keep-alive",
            }
        }
    }
    #[derive(Clone, Copy)]
    pub struct MioEvent {
        writable: bool,
        readable: bool,
        token: usize,
    }
    impl MioEvent {
        pub fn from_event(event: &Event) -> Self {
            Self {
                writable: event.is_writable(),
                readable: event.is_readable(),
                token: event.token().0,
            }
        }
        pub fn writable(&self) -> bool {
            self.writable
        }
        pub fn readable(&self) -> bool {
            self.readable
        }
        pub fn token(&self) -> Token {
            Token(self.token)
        }
        pub fn raw_token(&self) -> usize {
            self.token
        }
    }
    pub struct Connection {
        socket: TcpStream,
        token: Token,
        session: ServerSession,
        closing: bool,
    }
    impl Connection {
        pub fn new(socket: TcpStream, token: Token, session: ServerSession) -> Self {
            Self {
                socket,
                token,
                session,
                closing: false,
            }
        }

        pub fn ready(
            &mut self,
            registry: &Registry,
            event: &MioEvent,
            storage: &mut Storage,
            extensions: &mut ExtensionMap,
        ) {
            // If socket is readable, read from socket to session
            if event.readable() && self.decrypt().is_ok() {
                // Read request from session to buffer
                let (request, request_len) = {
                    let mut buffer = [0; 16_384_usize];
                    let len = {
                        let mut read = 0;
                        loop {
                            match self.session.read(&mut buffer) {
                                Err(ref err) if err.kind() == io::ErrorKind::ConnectionAborted => {
                                    self.close();
                                    break;
                                }
                                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => {
                                    continue
                                }
                                Err(err) => {
                                    eprintln!("Failed to read from session! {:?}", err);
                                    self.close();
                                    break;
                                }
                                Ok(0) => break,
                                Ok(rd) => read += rd,
                            }
                        }
                        read
                    };
                    (buffer, len)
                };

                // If not empty, parse and process it!
                if request_len > 0 {
                    let mut close = ConnectionHeader::KeepAlive;
                    if request_len == request.len() {
                        eprintln!("Request too large!");
                        let _ = default_error(413, &close, Some(storage.get_fs()))
                            .write_all(&mut self.session);
                    } else {
                        match parse::parse_request(&request[..request_len]) {
                            Ok(parsed) => {
                                // Get close header
                                close = ConnectionHeader::from_close({
                                    match parsed.headers().get("connection") {
                                        Some(connection) => {
                                            connection
                                                == http::header::HeaderValue::from_static("close")
                                        }
                                        None => false,
                                    }
                                });

                                match parsed.version() {
                                    Version::HTTP_11 => {
                                        if let Err(err) = process_request(
                                            &mut self.session,
                                            parsed,
                                            &request[..],
                                            &close,
                                            storage,
                                            extensions,
                                        ) {
                                            eprintln!("Failed to write to session! {:?}", err);
                                        };
                                        // Flush all contents, important for compression
                                        let _ = self.session.flush();
                                    }
                                    _ => {
                                        // Unsupported HTTP version!
                                        let _ = default_error(505, &close, Some(storage.get_fs()))
                                            .write_all(&mut self.session);
                                    }
                                }
                            }
                            Err(err) => {
                                eprintln!(
                  "Failed to parse request, write something as a response? Err: {:?}",
                  err
                );
                                let _ = default_error(400, &close, Some(storage.get_fs()))
                                    .write_all(&mut self.session);
                            }
                        };
                    }

                    if close.close() {
                        self.session.send_close_notify();
                    };
                }
            }
            if event.writable() {
                match self.session.write_tls(&mut self.socket) {
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                        // If the whole message couldn't be transmitted in one round
                    }
                    Err(err) => {
                        eprintln!("Error writing to socket! {:?}", err,);
                        self.close();
                    }
                    // Do nothing!
                    Ok(..) => {}
                }
            }

            if self.closing {
                println!("Closing connection!");
                let _ = self.socket.shutdown(std::net::Shutdown::Both);
                self.deregister(registry);
            } else {
                self.reregister(registry);
            };
        }
        fn decrypt(&mut self) -> Result<(), ()> {
            // Loop on read_tls
            match self.session.read_tls(&mut self.socket) {
                Err(err) => {
                    if let io::ErrorKind::WouldBlock = err.kind() {
                        eprintln!("Would block!");
                        return Err(());
                    } else {
                        self.close();
                        return Err(());
                    }
                }
                Ok(0) => {
                    self.close();
                    return Err(());
                }
                _ => {
                    if let Err(err) = self.session.process_new_packets() {
                        eprintln!("Failed to process packets {}", err);
                        self.close();
                        return Err(());
                    };
                }
            };
            Ok(())
        }

        #[inline]
        pub fn register(&mut self, registry: &Registry) {
            let es = self.event_set();
            registry
                .register(&mut self.socket, self.token, es)
                .expect("Failed to register connection!");
        }
        #[inline]
        pub fn reregister(&mut self, registry: &Registry) {
            let es = self.event_set();
            registry
                .reregister(&mut self.socket, self.token, es)
                .expect("Failed to register connection!");
        }
        #[inline]
        pub fn deregister(&mut self, registry: &Registry) {
            registry
                .deregister(&mut self.socket)
                .expect("Failed to register connection!");
        }

        fn event_set(&self) -> Interest {
            let rd = self.session.wants_read();
            let wr = self.session.wants_write();

            if rd && wr {
                Interest::READABLE | Interest::WRITABLE
            } else if wr {
                Interest::WRITABLE
            } else {
                Interest::READABLE
            }
        }

        #[inline]
        pub fn is_closed(&self) -> bool {
            self.closing
        }
        #[inline]
        fn close(&mut self) {
            self.closing = true;
        }
    }
}

pub mod bindings {
    use super::{mime_guess, Cached, Cow, FsCache, Mime};
    use http::Request;
    use std::collections::HashMap;

    pub enum ContentType {
        FromMime(Mime),
        Html,
        PlainText,
        Download,
        AutoOrDownload,
        AutoOrPlain,
        AutoOrHTML,
    }
    impl ContentType {
        pub fn as_str<P: AsRef<std::path::Path>>(&self, path: P) -> Cow<'static, str> {
            match self {
                ContentType::FromMime(mime) => Cow::Owned(format!("{}", mime)),
                ContentType::Html => Cow::Borrowed("text/html"),
                ContentType::PlainText => Cow::Borrowed("text/plain"),
                ContentType::Download => Cow::Borrowed("application/octet-stream"),
                ContentType::AutoOrDownload => Cow::Owned(format!(
                    "{}",
                    mime_guess::from_path(&path).first_or_octet_stream()
                )),
                ContentType::AutoOrPlain => Cow::Owned(format!(
                    "{}",
                    mime_guess::from_path(&path).first_or_text_plain()
                )),
                ContentType::AutoOrHTML => Cow::Owned(format!(
                    "{}",
                    mime_guess::from_path(&path).first_or(mime::TEXT_HTML)
                )),
            }
        }
    }
    impl Default for ContentType {
        fn default() -> Self {
            Self::AutoOrDownload
        }
    }

    type Binding =
        dyn Fn(&mut Vec<u8>, &Request<&[u8]>, &mut FsCache) -> (ContentType, Cached) + Send + Sync;

    /// Function bindings to have fast dynamic pages.
    ///
    /// Functions can be associated with URLs by calling the `bind` function.
    pub struct FunctionBindings {
        page_map: HashMap<String, Box<Binding>>,
        dir_map: HashMap<String, Box<Binding>>,
    }
    impl FunctionBindings {
        /// Creates a new, empty set of bindings.
        ///
        /// Use `bind` to populate it
        #[inline]
        pub fn new() -> Self {
            FunctionBindings {
                page_map: HashMap::new(),
                dir_map: HashMap::new(),
            }
        }
        /// Binds a function to a path. Case sensitive.
        /// Don't forget to handle methods other than `GET`. `HEAD` is implemented in the backend.
        ///
        /// Fn needs to return a tuple with the content type (e.g. `text/html`), and whether the return value should be cached or not.
        /// # Examples
        /// ```
        /// use arktis::{FunctionBindings, ContentType, write_error, Cached};
        ///
        /// let mut bindings = FunctionBindings::new();
        ///
        /// bindings.bind_page("/test", |buffer, request, _| {
        ///    buffer.extend(b"<h1>Welcome to my site!</h1> You are calling: ".iter());
        ///    buffer.extend(format!("{}", request.uri()).as_bytes());
        ///
        ///    (ContentType::Html, Cached::Static)
        /// });
        /// bindings.bind_page("/throw_500", |mut buffer, _, storage| {
        ///   write_error(&mut buffer, 500, storage);
        ///
        ///   (ContentType::Html, Cached::Changing)
        /// });
        /// ```
        #[inline]
        pub fn bind_page<F>(&mut self, path: &str, callback: F)
        where
            F: Fn(&mut Vec<u8>, &Request<&[u8]>, &mut FsCache) -> (ContentType, Cached)
                + 'static
                + Send
                + Sync,
        {
            self.page_map.insert(path.to_owned(), Box::new(callback));
        }
        /// Unbinds a function from a page.
        ///
        /// Returns `None` if path wasn't bind.
        #[inline]
        pub fn unbind_page(&mut self, path: &str) -> Option<()> {
            self.page_map.remove(path).and(Some(()))
        }

        /// Binds a function to a directory; if the requests path starts with any entry, it gets directed to the associated function. Case sensitive.
        /// Don't forget to handle methods other than `GET`. `HEAD` is implemented in the backend.
        ///
        /// Fn needs to return a tuple with the content type (e.g. `text/html`), and whether the return value should be cached or not.
        /// # Examples
        /// ```
        /// use arktis::{FunctionBindings, ContentType, Cached};
        /// use http::Method;
        ///
        /// let mut bindings = FunctionBindings::new();
        ///
        /// bindings.bind_dir("/api/v1", |buffer, request, _| {
        ///    buffer.extend(b"<h1>Welcome to my <i>new</i> <b>API</b>!</h1> You are calling: ".iter());
        ///    buffer.extend(format!("{}", request.uri()).as_bytes());
        ///
        ///    (ContentType::Html, Cached::Dynamic)
        /// });
        /// ```
        #[inline]
        pub fn bind_dir<F>(&mut self, path: &str, callback: F)
        where
            F: Fn(&mut Vec<u8>, &Request<&[u8]>, &mut FsCache) -> (ContentType, Cached)
                + 'static
                + Send
                + Sync,
        {
            self.dir_map.insert(path.to_owned(), Box::new(callback));
        }
        /// Unbinds a function from a directory.
        ///
        /// Returns None if path wasn't bind.
        #[inline]
        pub fn unbind_dir(&mut self, path: &str) -> Option<()> {
            self.dir_map.remove(path).and(Some(()))
        }

        /// Gets the function associated with the URL, if there is one.
        #[inline]
        pub fn get_binding(&self, path: &str) -> Option<&Box<Binding>> {
            self.page_map.get(path).or_else(|| {
                for (binding_path, binding_fn) in self.dir_map.iter() {
                    if path.starts_with(binding_path.as_str()) {
                        return Some(binding_fn);
                    }
                }
                None
            })
        }
    }
}

pub mod tls_server_config {
    use rustls::{internal::pemfile, NoClientAuth, ServerConfig};
    use std::{
        fs::File,
        io::{self, BufReader},
        path::Path,
    };

    #[derive(Debug)]
    pub enum ServerConfigError {
        IO(io::Error),
        ImproperPrivateKeyFormat,
        ImproperCertificateFormat,
        NoKey,
        InvalidPrivateKey,
    }
    impl From<io::Error> for ServerConfigError {
        fn from(error: io::Error) -> Self {
            Self::IO(error)
        }
    }
    pub fn get_server_config<P: AsRef<Path>>(
        cert_path: P,
        private_key_path: P,
    ) -> Result<ServerConfig, ServerConfigError> {
        let mut chain = BufReader::new(File::open(&cert_path)?);
        let mut private_key = BufReader::new(File::open(&private_key_path)?);

        let mut server_config = ServerConfig::new(NoClientAuth::new());
        let mut private_keys = Vec::with_capacity(4);
        private_keys.extend(match pemfile::pkcs8_private_keys(&mut private_key) {
            Ok(key) => key,
            Err(()) => return Err(ServerConfigError::ImproperPrivateKeyFormat),
        });
        private_keys.extend(match pemfile::rsa_private_keys(&mut private_key) {
            Ok(key) => key,
            Err(()) => return Err(ServerConfigError::ImproperPrivateKeyFormat),
        });
        if let Err(..) = server_config.set_single_cert(
            match pemfile::certs(&mut chain) {
                Ok(cert) => cert,
                Err(()) => return Err(ServerConfigError::ImproperCertificateFormat),
            },
            match private_keys.into_iter().next() {
                Some(key) => key,
                None => return Err(ServerConfigError::NoKey),
            },
        ) {
            Err(ServerConfigError::InvalidPrivateKey)
        } else {
            Ok(server_config)
        }
    }
}

#[allow(dead_code)]
mod stack_buffered_write {
    use std::io::{self, Write};

    const BUFFER_SIZE: usize = 8192;
    // const BUFFER_SIZE: usize = 8;
    pub struct Buffered<'a, W: Write> {
        buffer: [u8; BUFFER_SIZE],
        // Must not be more than buffer.len()
        index: usize,
        writer: &'a mut W,
    }
    impl<'a, W: Write> Buffered<'a, W> {
        pub fn new(writer: &'a mut W) -> Self {
            Self {
                buffer: [0; BUFFER_SIZE],
                index: 0,
                writer,
            }
        }

        #[inline]
        pub fn left(&self) -> usize {
            self.buffer.len() - self.index
        }

        pub fn write(&mut self, buf: &[u8]) -> io::Result<()> {
            if buf.len() > self.left() {
                if buf.len() + self.index < self.buffer.len() * 2 {
                    let copy = self.left();
                    self.buffer[self.index..].copy_from_slice(&buf[..copy]);
                    unsafe {
                        self.flush_all()?;
                    }
                    self.buffer[..buf.len() - copy].copy_from_slice(&buf[copy..]);
                    self.index = buf.len() - copy;

                    self.try_flush()?;
                } else {
                    self.flush_remaining()?;
                    self.writer.write_all(buf)?;
                }
            } else {
                self.buffer[self.index..self.index + buf.len()].copy_from_slice(buf);
                self.index += buf.len();

                self.try_flush()?;
            }
            Ok(())
        }
        #[inline]
        pub unsafe fn flush_all(&mut self) -> io::Result<()> {
            self.index = 0;
            self.writer.write_all(&self.buffer[..])
        }
        pub fn flush_remaining(&mut self) -> io::Result<()> {
            self.writer.write_all(&self.buffer[..self.index])?;
            self.index = 0;
            Ok(())
        }
        pub fn try_flush(&mut self) -> io::Result<()> {
            if self.index == self.buffer.len() {
                unsafe {
                    self.flush_all()?;
                }
            }
            Ok(())
        }

        #[inline]
        pub fn inner(&mut self) -> &mut W {
            &mut self.writer
        }
    }
    impl<'a, W: Write> Drop for Buffered<'a, W> {
        fn drop(&mut self) {
            let _ = self.flush_remaining();
        }
    }
    impl<'a, W: Write> Write for Buffered<'a, W> {
        #[inline]
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.write(buf)?;
            Ok(buf.len())
        }
        #[inline]
        fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
            self.write(buf)
        }
        #[inline]
        fn flush(&mut self) -> io::Result<()> {
            self.flush_remaining()
        }
    }
}
