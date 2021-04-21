//! Utility functions to provide solid solutions for common problems.
//!
//! This includes
//! - [`WriteableBytes`] for when you want to use [`Bytes`]
//!   as a [`Vec`], with tailored allocations
//! - [`CleanDebug`] to get the [`Display`] implementation when
//!   implementing [`Debug`] for a struct (see the Debug implementation for [`Host`])
//! - Async cached access to the file system
//! - Default errors which can be customised in `<host_dir>/errors/<status_code>.html`
//! - And several [`http`] helper functions.

use crate::prelude::{fs::*, *};

/// Common characters expressed as a single byte each, according to UTF-8.
pub mod chars {
    /// Tab
    pub const TAB: u8 = 9;
    /// Line feed
    pub const LF: u8 = 10;
    /// Carrage return
    pub const CR: u8 = 13;
    /// ` `
    pub const SPACE: u8 = 32;
    /// `!`
    pub const BANG: u8 = 33;
    /// `&`
    pub const AMPERSAND: u8 = 38;
    /// `.`
    pub const PERIOD: u8 = 46;
    /// `/`
    pub const FORWARD_SLASH: u8 = 47;
    /// `:`
    pub const COLON: u8 = 58;
    /// `>`
    pub const PIPE: u8 = 62;
    /// `[`
    pub const L_SQ_BRACKET: u8 = 91;
    /// `\`
    pub const ESCAPE: u8 = 92;
    /// `]`
    pub const R_SQ_BRACKET: u8 = 93;
}

/// Conveniency macro to create a [`Bytes`] from multiple `&[u8]` sources.
///
/// Allocates only once; capacity is calculated before any allocation.
///
/// Works like the [`vec!`] macro, but takes byte slices and concatenates them together.
#[macro_export]
macro_rules! build_bytes {
    () => (
        $crate::prelude::Bytes::new()
    );
    ($($bytes:expr),+ $(,)?) => {{
        let mut b = $crate::prelude::BytesMut::with_capacity($($bytes.len() +)* 0);

        $(b.extend($bytes.iter());)*

        b.freeze()
    }};
}

/// A writeable `Bytes`.
///
/// Has a special allocation method for optimized usage in Kvarn.
#[derive(Debug)]
#[must_use]
pub struct WriteableBytes {
    bytes: BytesMut,
    len: usize,
}
impl WriteableBytes {
    /// Creates a new writeable buffer. Consider using
    /// [`Self::with_capacity()`] if you can estimate the capacity needed.
    #[inline]
    pub fn new() -> Self {
        Self {
            bytes: BytesMut::new(),
            len: 0,
        }
    }
    /// Crates a new writeable buffer with a specified capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        let mut bytes = BytesMut::with_capacity(capacity);
        // This is safe because of the guarantees of `WriteableBytes`; it stores the length internally
        // and applies it when the inner variable is exposed, through `Self::into_inner()`.
        unsafe { bytes.set_len(bytes.capacity()) };
        Self { bytes, len: 0 }
    }
    /// Turns `self` into `BytesMut` when you are done writing.
    #[inline]
    #[must_use]
    pub fn into_inner(mut self) -> BytesMut {
        unsafe { self.bytes.set_len(self.len) };
        self.bytes
    }
}
impl Default for WriteableBytes {
    fn default() -> Self {
        Self::new()
    }
}
impl From<BytesMut> for WriteableBytes {
    fn from(mut bytes: BytesMut) -> Self {
        let len = bytes.len();
        // This is safe because of the guarantees of `WriteableBytes`; it stores the length internally
        // and applies it when the inner variable is exposed, through `Self::into_inner()`.
        unsafe { bytes.set_len(bytes.capacity()) };
        Self { bytes, len }
    }
}
impl Write for WriteableBytes {
    #[inline]
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.len + buf.len() > self.bytes.capacity() {
            self.bytes.reserve(buf.len() + 512);
            // This is safe because of the guarantees of `WriteableBytes`; it stores the length internally
            // and applies it when the inner variable is exposed, through `Self::into_inner()`.
            unsafe { self.bytes.set_len(self.bytes.capacity()) };
        }
        self.bytes[self.len..self.len + buf.len()].copy_from_slice(buf);
        self.len += buf.len();
        Ok(buf.len())
    }
    #[inline]
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// `ToDo`: optimize!
///
///
/// # Errors
///
/// This function will return any errors emitted from `reader`.
pub async fn read_to_end<R: AsyncRead + Unpin>(
    buffer: &mut BytesMut,
    mut reader: R,
) -> io::Result<()> {
    let mut read = buffer.len();
    // This is safe because of the trailing unsafe block.
    unsafe { buffer.set_len(buffer.capacity()) };
    loop {
        match reader.read(&mut buffer[read..]).await? {
            0 => break,
            len => {
                read += len;
                if read > buffer.len() - 512 {
                    buffer.reserve(2048);
                    // This is safe because of the trailing unsafe block.
                    unsafe { buffer.set_len(buffer.capacity()) };
                }
            }
        }
    }
    // I have counted the length in `read`. It will *not* include uninitiated bytes.
    unsafe { buffer.set_len(read) };
    Ok(())
}

/// Reads a file using a `cache`.
/// Should be used instead of [`fs::File::open()`].
///
/// Should only be used when a file is typically access several times or from several requests.
#[cfg(not(feature = "no-fs-cache"))]
#[inline]
pub async fn read_file_cached<P: AsRef<Path>>(path: &P, cache: &FileCache) -> Option<Bytes> {
    if let Some(file) = cache.lock().await.get(path.as_ref()) {
        return Some(Bytes::clone(file));
    }

    let file = File::open(path).await.ok()?;
    let mut buffer = BytesMut::with_capacity(4096);
    read_to_end(&mut buffer, file).await.ok()?;
    let buffer = buffer.freeze();
    cache
        .lock()
        .await
        .cache(path.as_ref().to_path_buf(), Bytes::clone(&buffer));
    Some(buffer)
}
/// Reads a file using a `cache`.
/// Should be used instead of [`fs::File::open()`].
///
/// Should only be used when a file is typically access several times or from several requests.
#[cfg(feature = "no-fs-cache")]
#[inline]
pub async fn read_file_cached<P: AsRef<Path>>(path: &P, _: &FileCache) -> Option<Bytes> {
    let file = File::open(path).await.ok()?;
    let mut buffer = BytesMut::with_capacity(4096);
    read_to_end(&mut buffer, file).await.ok()?;
    Some(buffer.freeze())
}

/// Reads a file using a `cache`.
/// Should be used instead of [`fs::File::open()`].
///
/// Should be used when a file is typically only accessed once, and cached in the response cache, not files multiple requests often access.
#[cfg(not(feature = "no-fs-cache"))]
#[inline]
pub async fn read_file<P: AsRef<Path>>(path: &P, cache: &FileCache) -> Option<Bytes> {
    if let Some(cached) = cache.lock().await.get(path.as_ref()) {
        return Some(Bytes::clone(cached));
    }

    let file = File::open(path).await.ok()?;
    let mut buffer = BytesMut::with_capacity(4096);
    read_to_end(&mut buffer, file).await.ok()?;
    Some(buffer.freeze())
}
/// Reads a file using a `cache`.
/// Should be used instead of [`fs::File::open()`].
///
/// Should be used when a file is typically only accessed once, and cached in the response cache, not files multiple requests often access.
#[cfg(feature = "no-fs-cache")]
#[inline]
pub async fn read_file<P: AsRef<Path>>(path: &P, _: &FileCache) -> Option<Bytes> {
    let file = File::open(path).await.ok()?;
    let mut buffer = BytesMut::with_capacity(4096);
    read_to_end(&mut buffer, file).await.ok()?;
    Some(buffer.freeze())
}

/// Makes a [`PathBuf`] using one allocation.
///
/// Format is `<base_path>/<dir>/<file>(.<extension>)`
pub fn make_path(
    base_path: impl AsRef<Path>,
    dir: impl AsRef<Path>,
    file: impl AsRef<Path>,
    extension: Option<&str>,
) -> PathBuf {
    let mut path = PathBuf::with_capacity(
        base_path.as_ref().as_os_str().len()
            + dir.as_ref().as_os_str().len()
            + 2
            + file.as_ref().as_os_str().len()
            + extension.map_or(0, |e| e.len() + 1),
    );
    path.push(base_path);
    path.push(dir);
    path.push(file);
    if let Some(extension) = extension {
        path.set_extension(extension);
    }
    path
}

/// Get a hardcoded error message.
///
/// It can be useful when you don't have access to the file cache
/// or if a error html file isn't provided. Is used by the preferred
/// function [`default_error`].
#[must_use]
pub fn hardcoded_error_body(code: StatusCode, message: Option<&[u8]>) -> Bytes {
    // a 404 page is 168 bytes. Accounting for long code.canonical_reason() and future message.
    let mut body = BytesMut::with_capacity(200);
    // Get code and reason!
    let reason = code.canonical_reason();

    body.extend(b"<html><head><title>");
    // Code and reason
    body.extend(code.as_str().as_bytes());
    body.extend(b" ");
    if let Some(reason) = reason {
        body.extend(reason.as_bytes());
    }

    body.extend(b"</title></head><body><center><h1>".iter());
    // Code and reason
    body.extend(code.as_str().as_bytes());
    body.extend(b" ");
    if let Some(reason) = reason {
        body.extend(reason.as_bytes());
    }
    body.extend(b"</h1><hr>An unexpected error occurred. <a href='/'>Return home</a>?".iter());

    if let Some(message) = message {
        body.extend(b"<p>");
        body.extend(message);
        body.extend(b"</p>");
    }

    body.extend(b"</center></body></html>".iter());

    body.freeze()
}

/// Default HTTP error used in Kvarn.
///
/// Gets the default error based on `code` from the file system
/// through a cache.
#[inline]
#[allow(clippy::missing_panics_doc)]
pub async fn default_error(
    code: StatusCode,
    host: Option<&Host>,
    message: Option<&[u8]>,
) -> Response<Bytes> {
    // Error files will be used several times.
    let body = match host {
        Some(host) => {
            let path = make_path(&host.path, "errors", code.as_str(), Some("html"));

            match read_file_cached(&path, &host.file_cache).await {
                Some(file) => file,
                None => hardcoded_error_body(code, message),
            }
        }
        None => hardcoded_error_body(code, message),
    };
    let mut builder = Response::builder()
        .status(code)
        .header("content-type", "text/html; charset=utf-8")
        .header("content-encoding", "identity");
    if let Some(message) = message.map(HeaderValue::from_bytes).and_then(Result::ok) {
        builder = builder.header("reason", message);
    }
    // Unwrap is ok; I know it's valid
    builder.body(body).unwrap()
}

/// Get a error [`FatResponse`].
///
/// Can be very useful to return from [`extensions`].
#[inline]
pub async fn default_error_response(
    code: StatusCode,
    host: &Host,
    message: Option<&str>,
) -> FatResponse {
    (
        default_error(code, Some(host), message.map(str::as_bytes)).await,
        ClientCachePreference::Full,
        ServerCachePreference::None,
        CompressPreference::Full,
    )
}

/// Clones a [`Response`], discarding the body.
///
/// Use [`Response::map()`] to add a body.
#[inline]
#[allow(clippy::missing_panics_doc)]
pub fn empty_clone_response<T>(response: &Response<T>) -> Response<()> {
    let mut builder = Response::builder()
        .version(response.version())
        .status(response.status());

    // Unwrap is ok, the builder is guaranteed to have a [`HeaderMap`] if it's valid, which we know it is from above.
    *builder.headers_mut().unwrap() = response.headers().clone();
    builder.body(()).unwrap()
}
/// Clones a [`Request`], discarding the body.
///
/// Use [`Request::map()`] to add a body.
#[inline]
#[allow(clippy::missing_panics_doc)]
pub fn empty_clone_request<T>(request: &Request<T>) -> Request<()> {
    let mut builder = Request::builder()
        .method(request.method())
        .version(request.version())
        .uri(request.uri().clone());
    // Unwrap is ok, the builder is guaranteed to have a [`HeaderMap`] if it's valid, which we know it is from above.
    *builder.headers_mut().unwrap() = request.headers().clone();
    builder.body(()).unwrap()
}
/// Splits a [`Response`] into a empty [`Response`] and it's body.
#[inline]
#[allow(clippy::missing_panics_doc)]
pub fn split_response<T>(response: Response<T>) -> (Response<()>, T) {
    let mut body = None;
    let response = response.map(|t| body = Some(t));
    // We know it is `Some`.
    (response, body.unwrap())
}

/// Replaces the header `name` with `new` in `headers`.
///
/// Removes all other occurrences of `name`.
#[inline]
pub fn replace_header<K: header::IntoHeaderName + Copy>(
    headers: &mut HeaderMap,
    name: K,
    new: HeaderValue,
) {
    match headers.entry(name) {
        header::Entry::Vacant(slot) => {
            slot.insert(new);
        }
        header::Entry::Occupied(slot) => {
            slot.remove_entry_mult();
            headers.insert(name, new);
        }
    }
}
/// Replaces header `name` with `new` (a &'static str) in `headers`.
///
/// See [`replace_header`] for more info.
#[inline]
pub fn replace_header_static<K: header::IntoHeaderName + Copy>(
    headers: &mut HeaderMap,
    name: K,
    new: &'static str,
) {
    replace_header(headers, name, HeaderValue::from_static(new))
}

/// Check if `bytes` starts with a valid [`Method`].
#[must_use]
pub fn valid_method(bytes: &[u8]) -> bool {
    bytes.starts_with(b"GET")
        || bytes.starts_with(b"HEAD")
        || bytes.starts_with(b"POST")
        || bytes.starts_with(b"PUT")
        || bytes.starts_with(b"DELETE")
        || bytes.starts_with(b"TRACE")
        || bytes.starts_with(b"OPTIONS")
        || bytes.starts_with(b"CONNECT")
        || bytes.starts_with(b"PATCH")
}
/// Gets the `content-length` from the [`Request::headers`] of `request`.
///
/// If <code>!\[`method_has_request_body`]</code> or the header isn't present, it defaults to `0`.
#[inline]
pub fn get_content_length<T>(request: &Request<T>) -> usize {
    use std::str::FromStr;
    if method_has_request_body(request.method()) {
        request
            .headers()
            .get("content-length")
            .map(HeaderValue::to_str)
            .and_then(Result::ok)
            .map(usize::from_str)
            .and_then(Result::ok)
            .unwrap_or(0)
    } else {
        0
    }
}
/// Sets the `content-length` of `headers` to `len`.
///
/// See [`replace_header`] for details.
#[inline]
#[allow(clippy::missing_panics_doc)]
pub fn set_content_length(headers: &mut HeaderMap, len: usize) {
    // unwrap is ok, we know the formatted bytes from a number are (0-9) or `.`
    utility::replace_header(
        headers,
        "content-length",
        HeaderValue::from_str(len.to_string().as_str()).unwrap(),
    )
}

/// Does a request of type `method` have a body?
#[inline]
#[must_use]
pub fn method_has_request_body(method: &Method) -> bool {
    matches!(*method, Method::POST | Method::PUT | Method::DELETE)
}
/// Does a response of type `method` have a body?
#[inline]
#[must_use]
pub fn method_has_response_body(method: &Method) -> bool {
    matches!(
        *method,
        Method::GET
            | Method::POST
            | Method::DELETE
            | Method::CONNECT
            | Method::OPTIONS
            | Method::PATCH
    )
}

/// Implements [`Debug`] from the [`Display`] implementation of `value`.
///
/// Can be used to give fields a arbitrary [`str`] without surrounding quotes.
pub struct CleanDebug<'a, T: ?Sized + Display>(&'a T);
impl<'a, T: ?Sized + Display> CleanDebug<'a, T> {
    /// Creates a new wrapper around `value` with [`Debug`] implemented as [`Display`].
    #[inline]
    pub fn new(value: &'a T) -> Self {
        Self(value)
    }
}
impl<'a, T: ?Sized + Display> Debug for CleanDebug<'a, T> {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self.0, f)
    }
}
impl<'a, T: ?Sized + Display> Display for CleanDebug<'a, T> {
    #[inline]
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(self.0, f)
    }
}
