use mio::net::{TcpListener, TcpStream};
use std::borrow::Cow;
use std::net;
use std::path::PathBuf;
use std::sync::{self, Arc, Mutex};
use std::{
  fs::File,
  io::{self, prelude::*},
};

pub use bindings::FunctionBindings;
pub use cache::*;
pub use chars::*;
pub use connection::Connection;

mod extensions;
mod threading;

const HTTPS_SERVER: mio::Token = mio::Token(0);
const RESERVED_TOKENS: usize = 1024;
#[cfg(windows)]
const SERVER_HEADER: &[u8] = b"Server: Arktis/0.1.0 (Windows)\r\n";
#[cfg(unix)]
const SERVER_HEADER: &[u8] = b"Server: Arktis/0.1.0 (Unix)\r\n";
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

  /// Runs a server from the config on a new thread, not blocking the current thread.
  ///
  /// Use a loop to capture the main thread.
  ///
  /// # Examples
  /// ```
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
    poll
      .registry()
      .register(&mut self.socket, HTTPS_SERVER, mio::Interest::READABLE)
      .expect("Failed to register HTTPS server");

    let mut thread_handler =
      threading::HandlerPool::new(self.clone_inner(), self.clone_storage(), poll.registry());

    loop {
      poll.poll(&mut events, None).expect("Failed to poll!");

      for event in events.iter() {
        match event.token() {
          HTTPS_SERVER => {
            self
              .accept(&mut thread_handler)
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
    Storage {
      fs: Arc::new(Mutex::new(Cache::new())),
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
    Storage {
      fs: Arc::new(Mutex::new(Cache::new())),
      response: Arc::new(Mutex::new(Cache::new())),
      template: Arc::new(Mutex::new(Cache::with_max(128))),
      bindings,
    }
  }

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
    #[cfg(feature = "no-cache")]
    return None;
    #[cfg(not(feature = "no-cache"))]
    match self.fs.try_lock() {
      Ok(lock) => Some(lock),
      Err(ref err) => match err {
        sync::TryLockError::WouldBlock => None,
        sync::TryLockError::Poisoned(..) => panic!("Lock is poisoned!"),
      },
    }
  }
  /// Tries to get the lock of response cache.
  ///
  /// Always remember to handle the case if the lock isn't acquired; just don't return None!
  #[inline]
  pub fn try_response(&mut self) -> Option<sync::MutexGuard<'_, ResponseCacheInner>> {
    #[cfg(feature = "no-cache")]
    return None;
    #[cfg(not(feature = "no-cache"))]
    match self.response.try_lock() {
      Ok(lock) => Some(lock),
      Err(ref err) => match err {
        sync::TryLockError::WouldBlock => None,
        sync::TryLockError::Poisoned(..) => panic!("Lock is poisoned!"),
      },
    }
  }
  /// Tries to get the lock of template cache.
  ///
  /// Always remember to handle the case if the lock isn't acquired; just don't return None!
  #[inline]
  pub fn try_template(&mut self) -> Option<sync::MutexGuard<'_, TemplateCacheInner>> {
    #[cfg(feature = "no-cache")]
    return None;
    #[cfg(not(feature = "no-cache"))]
    match self.template.try_lock() {
      Ok(lock) => Some(lock),
      Err(ref err) => match err {
        sync::TryLockError::WouldBlock => None,
        sync::TryLockError::Poisoned(..) => panic!("Lock is poisoned!"),
      },
    }
  }
  #[inline]
  pub fn get_bindings(&mut self) -> &Bindings {
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

#[allow(unused_variables)]
fn process_request<W: Write>(
  socket: &mut W,
  request: http::Request<&[u8]>,
  raw_request: &[u8],
  close: &connection::ConnectionHeader,
  storage: &mut Storage,
) -> Result<(), io::Error> {
  // println!("Got request: {:?}", &request);
  // Load from cache
  // Try get response cache lock
  if let Some(lock) = storage.try_response() {
    // If response is in cache
    if let Some(response) = lock.get(request.uri()) {
      // println!("Getting from cache!");
      socket.write_all(&response[..])?;
      return Ok(());
    }
  }

  let mut write_headers = true;
  let mut do_cache = true;
  // Get from function or cache, to enable processing (extensions) from functions!
  let path = match parse::convert_uri(request.uri()) {
    Ok(path) => path,
    Err(()) => {
      socket.write_all(&default_error(403, close, Some(storage))[..])?;
      return Ok(());
    }
  };

  // PHP needs it, so don't give a warning!
  #[allow(unused_mut)]
  let (mut body, content_type) = match storage.get_bindings().get(request.uri().path()) {
    // We've got an function, call it and return body and result!
    Some(callback) => {
      let mut response = Vec::with_capacity(2048);
      let (content_type, cache) = callback(&mut response, &request);
      do_cache = cache;
      // Check if callback contains headers
      if &response[..5] == b"HTTP/" {
        write_headers = false;
      }
      (Arc::new(response), Cow::Borrowed(content_type))
    }
    // No function, try read from FS cache.
    None => {
      // Body
      let body = match read_file(&path, storage) {
        Some(response) => response,
        None => {
          socket.write_all(&default_error(404, close, Some(storage))[..])?;
          return Ok(());
        }
      };
      if request.uri().query().is_some() {
        do_cache = false;
      }
      // Content mime type
      let content_type = format!("{}", mime_guess::from_path(&path).first_or_octet_stream());
      (body, Cow::Owned(content_type))
    }
  };
  // Read file etc...
  let mut bytes = body.iter();
  // If file starts with "!>", meaning it's an extension-dependent file!
  if bytes.next() == Some(&BANG) && bytes.next() == Some(&PIPE) {
    // Get extention arguments
    let (extension_args, content_start) = {
      let mut args = Vec::with_capacity(8);
      let mut last_break = 2;
      let mut current_index = 2;
      for byte in bytes {
        if *byte == LF {
          if current_index - last_break > 1 {
            args.push(
              &body[last_break..if body.get(current_index - 1) == Some(&CR) {
                current_index - 1
              } else {
                current_index
              }],
            );
          }
          break;
        }
        if *byte == SPACE && current_index - last_break > 1 {
          args.push(&body[last_break..current_index]);
        }
        current_index += 1;
        if *byte == SPACE {
          last_break = current_index;
        }
      }
      // Plus one, since loop breaks before
      (args, current_index + 1)
    };

    if let Some(test) = extension_args.get(0) {
      match test {
        #[cfg(feature = "php")]
        &b"php" => {
          println!("Handling php!");
          match extensions::php(socket, raw_request, &path) {
            Ok(()) => {
              // Don't write headers!
              write_headers = false;
              // Check cache settings
              do_cache = extension_args
                .get(1)
                .and_then(|arg| Some(arg != b"false" && arg != b"no-cache" && arg != b"nocache"))
                .unwrap_or(true);
            }
            _ => {}
          };
        }
        #[cfg(feature = "templates")]
        &b"tmpl" if extension_args.len() > 1 => {
          body = Arc::new(extensions::template(
            &extension_args[..],
            &body[content_start..],
            storage,
          ));
        }
        _ => {
          body = Arc::new(body[content_start..].to_vec());
        }
      }
    }
  };

  let response = if write_headers {
    let mut response = Vec::with_capacity(4096);
    response.extend(
      b"HTTP/1.1 200 OK\r\n\
        Connection: "
        .iter(),
    );
    if close.close() {
      response.extend(b"Close\r\n".iter());
    } else {
      response.extend(b"Keep-Alive\r\n".iter());
    }
    response.extend(b"Content-Length: ".iter());
    response.extend(format!("{}\r\n", body.len()).as_bytes());
    response.extend(b"Content-Type: ".iter());
    response.extend(content_type.as_bytes());
    response.extend(b"\r\n");
    // Temporary cache header
    // response.extend(b"Cache-Control: max-age=120\r\n");
    response.extend(b"Cache-Control: no-store\r\n");
    response.extend(SERVER_HEADER);
    response.extend(b"\r\n");
    response.extend(body.iter());

    socket.write_all(&response[..])?;
    Arc::new(response)
  } else {
    socket.write_all(&body[..])?;
    body
  };

  if do_cache {
    if let Some(mut lock) = storage.try_response() {
      let uri = request.into_parts().0.uri;
      println!("Caching uri {}", &uri);
      let _ = lock.cache(uri, response);
    }
  }
  Ok(())
}

fn default_error(
  code: u16,
  close: &connection::ConnectionHeader,
  storage: Option<&mut Storage>,
) -> Vec<u8> {
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

  fn get_default(code: u16) -> &'static [u8] {
    // Hard-coded defaults
    match code {
          404 => &b"<html><head><title>404 Not Found</title></head><body><center><h1>404 Not Found</h1><hr><a href='/'>Return home</a></center></body></html>"[..],
          _ => &b"<html><head><title>Unknown Error</title></head><body><center><h1>An unexpected error occurred, <a href='/'>return home</a>?</h1></center></body></html>"[..],
        }
  }

  match storage.and_then(|cache| read_file(&PathBuf::from(format!("{}.html", code)), cache)) {
    Some(file) => {
      buffer.extend(b"Content-Length: ");
      buffer.extend(format!("{}\r\n\r\n", file.len()).as_bytes());
      buffer.extend(&file[..]);
    }
    None => {
      let error = get_default(code);
      buffer.extend(b"Content-Length: ");
      buffer.extend(format!("{}\r\n\r\n", error.len()).as_bytes());
      buffer.extend(error);
    }
  };

  buffer
}
pub fn write_generic_error<W: Write>(writer: &mut W, code: u16) -> Result<(), io::Error> {
  writer.write_all(&default_error(code, &connection::ConnectionHeader::KeepAlive, None)[..])
}

fn read_file(path: &PathBuf, storage: &mut Storage) -> Option<Arc<Vec<u8>>> {
  if let Some(lock) = storage.try_fs() {
    if let Some(cached) = lock.get(path) {
      return Some(cached);
    }
  }

  match File::open(path) {
    Ok(mut file) => {
      let mut buffer = Vec::with_capacity(4096);
      match file.read_to_end(&mut buffer) {
        Ok(..) => {
          let buffer = Arc::new(buffer);
          match storage.try_fs() {
            Some(mut lock) => match lock.cache(path.clone(), buffer) {
              Err(failed) => Some(failed),
              Ok(()) => Some(lock.get(path).unwrap()),
            },
            None => Some(buffer),
          }
        }
        Err(..) => None,
      }
    }
    Err(..) => None,
  }
}

pub mod cache {
  use super::*;
  use http::Uri;
  use std::collections::HashMap;
  use std::{borrow::Borrow, hash::Hash};

  pub type FsCacheInner = Cache<PathBuf, Vec<u8>>;
  pub type FsCache = Arc<Mutex<FsCacheInner>>;
  pub type ResponseCacheInner = Cache<Uri, Vec<u8>>;
  pub type ResponseCache = Arc<Mutex<ResponseCacheInner>>;
  pub type TemplateCacheInner = Cache<String, HashMap<Arc<String>, Arc<Vec<u8>>>>;
  pub type TemplateCache = Arc<Mutex<TemplateCacheInner>>;
  pub type Bindings = Arc<FunctionBindings>;

  pub trait Size {
    fn count(&self) -> usize;
  }
  impl<T> Size for Vec<T> {
    fn count(&self) -> usize {
      self.len()
    }
  }
  impl<T> Size for dyn Borrow<Vec<T>> {
    fn count(&self) -> usize {
      self.borrow().len()
    }
  }
  impl<K, V> Size for HashMap<K, V> {
    fn count(&self) -> usize {
      self.len()
    }
  }
  impl<K, V> Size for dyn Borrow<HashMap<K, V>> {
    fn count(&self) -> usize {
      self.borrow().len()
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
      if value.count() > self.size_limit {
        return Err(value);
      }
      if self.map.len() >= self.max_items {
        // Reduce number of items!
        if let Some(last) = self
          .map
          .iter()
          .next()
          .and_then(|value| Some(value.0.clone()))
        {
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
        size_limit: 4194304,
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
      self.map.get(key).and_then(|value| Some(Arc::clone(value)))
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
  use mio::{event::Event, Interest, Registry, Token};
  use rustls::{ServerSession, Session};

  #[derive(PartialEq)]
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

    pub fn ready(&mut self, registry: &Registry, event: &MioEvent, storage: &mut Storage) {
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
                Err(ref err) if err.kind() == io::ErrorKind::Interrupted => continue,
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
            let _ = self
              .session
              .write_all(&default_error(413, &close, Some(storage))[..]);
          }

          match parse::parse_request(&request[..request_len]) {
            Ok(parsed) => {
              // Get close header
              close = ConnectionHeader::from_close({
                match parsed.headers().get("connection") {
                  Some(connection) => connection == http::header::HeaderValue::from_static("close"),
                  None => false,
                }
              });

              if let Err(err) =
                process_request(&mut self.session, parsed, &request[..], &close, storage)
              {
                eprintln!("Failed to write to session! {:?}", err);
              };
              // Flush all contents, important for compression
              let _ = self.session.flush();
            }
            Err(err) => {
              eprintln!(
                "Failed to parse request, write something as a response? Err: {:?}",
                err
              );
              let _ = self
                .session
                .write_all(&default_error(400, &close, Some(storage))[..]);
            }
          };

          if close.close() {
            self.session.send_close_notify();
          };
        }
      }
      if event.writable() {
        if let Err(..) = self.session.write_tls(&mut self.socket) {
          eprintln!("Error writing to socket!");
          self.close();
        };
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
          if self.session.process_new_packets().is_err() {
            eprintln!("Failed to process packets");
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
  use http::Request;
  use std::collections::HashMap;

  type Binding = dyn Fn(&mut Vec<u8>, &Request<&[u8]>) -> (&'static str, bool) + Send + Sync;

  /// Function bindings to have fast dynamic pages.
  ///
  /// Functions can be associated with URLs by calling the `bind` function.
  pub struct FunctionBindings {
    page_map: HashMap<
      String,
      Box<dyn Fn(&mut Vec<u8>, &Request<&[u8]>) -> (&'static str, bool) + Send + Sync>,
    >,
    dir_map: HashMap<
      String,
      Box<dyn Fn(&mut Vec<u8>, &Request<&[u8]>) -> (&'static str, bool) + Send + Sync>,
    >,
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
    ///
    /// Fn needs to return a tuple with the content type (e.g. "text/html"), and whether the return value should be cached or not.
    /// # Examples
    /// ```
    /// use arktis::{FunctionBindings, write_generic_error};
    ///
    /// let mut bindings = FunctionBindings::new();
    ///
    /// bindings.bind("/test", |buffer, request| {
    ///    buffer.extend(b"<h1>Welcome to my site!</h1> You are calling: ".iter());
    ///    buffer.extend(format!("{}", request.uri()).as_bytes());
    ///
    ///    ("text/html", true)
    /// });
    /// bindings.bind("/throw_500", |mut buffer, _| {
    ///   write_generic_error(&mut buffer, 500).expect("Failed to write to Vec!?");
    ///
    ///   ("text/html", false)
    /// });
    /// ```
    #[inline]
    pub fn bind<F>(&mut self, path: &str, callback: F)
    where
      F: Fn(&mut Vec<u8>, &Request<&[u8]>) -> (&'static str, bool) + 'static + Send + Sync,
    {
      self.page_map.insert(String::from(path), Box::new(callback));
    }
    /// Unbinds a function from a page.
    ///
    /// Returns None if path wasn't bind.
    #[inline]
    pub fn unbind(&mut self, path: &str) -> Option<()> {
      self.page_map.remove(path).and(Some(()))
    }

    /// Binds a function to a directory; if the requests path starts with any entry, it gets directed to the associated function. Case sensitive.
    ///
    /// Fn needs to return a tuple with the content type (e.g. "text/html"), and whether the return value should be cached or not.
    /// # Examples
    /// ```
    /// use arktis::FunctionBindings;
    ///
    /// let mut bindings = FunctionBindings::new();
    ///
    /// bindings.bind_dir("/api/v1", |buffer, request| {
    ///    buffer.extend(b"<h1>Welcome to my <i>new</i> <b>API</b>!</h1> You are calling: ".iter());
    ///    buffer.extend(format!("{}", request.uri()).as_bytes());
    ///
    ///    ("text/html", false)
    /// });
    /// ```
    #[inline]
    pub fn bind_dir<F>(&mut self, path: &str, callback: F)
    where
      F: Fn(&mut Vec<u8>, &Request<&[u8]>) -> (&'static str, bool) + 'static + Send + Sync,
    {
      self.dir_map.insert(String::from(path), Box::new(callback));
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
    pub fn get(&self, path: &str) -> Option<&Box<Binding>> {
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
pub mod parse {
  use http::{header::*, Method, Request, Uri, Version};
  use std::path::PathBuf;

  enum DecodeStage {
    Method,
    Path,
    Version,
    HeaderName(i32),
    HeaderValue(i32),
  }
  impl DecodeStage {
    fn next(&mut self) {
      *self = match self {
        DecodeStage::Method => DecodeStage::Path,
        DecodeStage::Path => DecodeStage::Version,
        DecodeStage::Version => DecodeStage::HeaderName(0),
        DecodeStage::HeaderName(n) => DecodeStage::HeaderValue(*n),
        DecodeStage::HeaderValue(n) => DecodeStage::HeaderName(*n + 1),
      }
    }
  }
  const LF: u8 = 10;
  const CR: u8 = 13;
  const SPACE: u8 = 32;
  const COLON: u8 = 58;
  pub fn parse_request(buffer: &[u8]) -> Result<Request<&[u8]>, http::Error> {
    let mut parse_stage = DecodeStage::Method;
    // Method is max 7 bytes long
    let mut method = [0; 7];
    let mut method_index = 0;
    let mut path = Vec::with_capacity(32);
    // Version is 8 bytes long
    let mut version = [0; 8];
    let mut version_index = 0;
    let mut parsed = Request::builder();
    let mut current_header_name = Vec::with_capacity(32);
    let mut current_header_value = Vec::with_capacity(128);
    let mut lf_in_row = 0;
    let mut last_header_byte = 0;
    for byte in buffer {
      last_header_byte += 1;
      if *byte == CR {
        continue;
      }
      if *byte == LF {
        lf_in_row += 1;
        if lf_in_row == 2 {
          break;
        }
      } else {
        lf_in_row = 0;
      }
      match parse_stage {
        DecodeStage::Method => {
          if *byte == SPACE || method_index == method.len() {
            parse_stage.next();
            continue;
          }
          method[method_index] = *byte;
          method_index += 1;
        }
        DecodeStage::Path => {
          if *byte == SPACE {
            parse_stage.next();
            continue;
          }
          path.push(*byte);
        }
        DecodeStage::Version => {
          if *byte == LF || version_index == version.len() {
            parse_stage.next();
            continue;
          }
          version[version_index] = *byte;
          version_index += 1;
        }
        DecodeStage::HeaderName(..) => {
          if *byte == COLON {
            continue;
          }
          if *byte == SPACE {
            parse_stage.next();
            continue;
          }
          current_header_name.push(*byte);
        }
        DecodeStage::HeaderValue(..) => {
          if *byte == LF {
            let name = HeaderName::from_bytes(&current_header_name[..]);
            let value = HeaderValue::from_bytes(&current_header_value[..]);
            if name.is_ok() && value.is_ok() {
              parsed = parsed.header(name.unwrap(), value.unwrap());
            }
            current_header_name.clear();
            current_header_value.clear();
            parse_stage.next();
            continue;
          }
          current_header_value.push(*byte);
        }
      };
    }
    parsed
      .method(Method::from_bytes(&method[..]).unwrap_or(Method::GET))
      .uri(Uri::from_maybe_shared(path).unwrap_or(Uri::from_static("/")))
      .version(match &version[..] {
        b"HTTP/0.9" => Version::HTTP_09,
        b"HTTP/1.0" => Version::HTTP_10,
        b"HTTP/1.1" => Version::HTTP_11,
        b"HTTP/2" => Version::HTTP_2,
        b"HTTP/2.0" => Version::HTTP_2,
        b"HTTP/3" => Version::HTTP_3,
        b"HTTP/3.0" => Version::HTTP_3,
        _ => Version::default(),
      })
      .body(&buffer[last_header_byte..])
  }

  pub fn convert_uri(uri: &Uri) -> Result<PathBuf, ()> {
    let mut path = uri.path();
    if path.contains("../") {
      return Err(());
    }
    let is_dir = path.ends_with("/");
    path = path.split_at(1).1;

    let mut buf = PathBuf::from("public");
    buf.push(path);
    if is_dir {
      buf.push("index.html");
    };
    Ok(buf)
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
