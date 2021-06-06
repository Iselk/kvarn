//! Abstractions for the [application layer](https://en.wikipedia.org/wiki/Application_layer),
//! providing a common interface for all HTTP versions supported.
//!
//! > **You should not have to interface with this module. Use [`handle_connection`] instead.**
//!
//! The main types are [`HttpConnection`], representing a single encrypted generic http connection.
//!
//! When accepting on the [`HttpConnection`], you get a [`FatRequest`]; a [`http::Request`] with a [`Body`].
//! The [`Body`] is a stream providing the body of a response if you need it, to avoid unnecessary allocations.
use crate::prelude::{internals::*, *};
pub use response::Http1Body;

/// General error for application-level logic.
///
/// Mostly, the [`Error::Parse`], [`Error::Io`], and [`Error::H2`]
/// signal errors with the request emitted from respective library.
#[derive(Debug)]
pub enum Error {
    /// A parse error from the module [`parse`].
    Parse(parse::Error),
    /// An input-output error was encountered while reading or writing.
    Io(io::Error),
    /// [`h2`] emitted an error
    #[cfg(feature = "h2")]
    H2(h2::Error),
    /// The HTTP version assumed by the client is not supported.
    /// Invalid ALPN config is a candidate.
    VersionNotSupported,
    /// You tried to push a response on a HTTP/1 connection.
    ///
    /// *Use HTTP/2 instead, or check if the [`ResponsePipe`] is HTTP/1*.
    PushOnHttp1,
}
impl From<parse::Error> for Error {
    #[inline]
    fn from(err: parse::Error) -> Self {
        Self::Parse(err)
    }
}
impl From<io::Error> for Error {
    #[inline]
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}
#[cfg(feature = "h2")]
impl From<h2::Error> for Error {
    #[inline]
    fn from(err: h2::Error) -> Self {
        Self::H2(err)
    }
}
impl From<Error> for io::Error {
    fn from(err: Error) -> io::Error {
        match err {
            Error::Parse(err) => err.into(),
            Error::Io(io) => io,
            #[cfg(feature = "h2")]
            Error::H2(h2) => io::Error::new(io::ErrorKind::InvalidData, h2),

            Error::VersionNotSupported => io::Error::new(
                io::ErrorKind::InvalidData,
                "http version unsupported. Invalid ALPN config.",
            ),
            Error::PushOnHttp1 => io::Error::new(
                io::ErrorKind::InvalidInput,
                "can not push requests on http/1",
            ),
        }
    }
}

/// A single HTTP connection.
///
/// See [`HttpConnection::new`] on how to make one and
/// [`HttpConnection::accept`] on getting a [`FatRequest`].
#[derive(Debug)]
pub enum HttpConnection {
    /// A HTTP/1 connection
    Http1(Arc<Mutex<Encryption>>),
    /// A HTTP/2 connection
    ///
    /// This is boxed because a [`h2::server::Connection`] takes up
    /// over 1000 bytes of memory, and an [`Arc`] 8 bytes.
    /// It will increase performance on servers with both HTTP/1 and HTTP/2
    /// connections, but slightly hurt exclusively HTTP/2 servers.
    ///
    /// We'll see how we move forward once HTTP/3 support lands.
    #[cfg(feature = "h2")]
    Http2(Box<h2::server::Connection<Encryption, bytes::Bytes>>),
}

/// A body of a [`Request`].
///
/// The inner variables are streams. To get the bytes, use [`Body::read_to_bytes()`] when needed.
///
/// Also see [`FatRequest`].
///
/// `ToDo`: trailers
#[derive(Debug)]
pub enum Body {
    /// An empty body.
    ///
    /// Can be used by HTTP/2 push to simulate a GET request.
    Empty,
    /// A buffered HTTP/1 body.
    ///
    /// While the HTTP/1 headers were read, it reads too much
    /// and some of the body will be read.
    /// Therefore, the already read bytes are stored.
    /// [`Body::read_to_bytes`] leverages this and just
    /// continues writing to the buffer.
    Http1(response::Http1Body<Encryption>),
    /// A HTTP/2 body provided by [`h2`].
    #[cfg(feature = "h2")]
    Http2(h2::RecvStream),
}

/// A pipe to send a [`Response`] through.
///
/// You may also push requests if the pipe is [`ResponsePipe::Http2`]
/// by calling [`ResponsePipe::push_request`].
#[derive(Debug)]
#[must_use]
pub enum ResponsePipe {
    /// A HTTP/1 stream to send a response.
    Http1(Arc<Mutex<Encryption>>),
    /// A HTTP/2 response pipe.
    #[cfg(feature = "h2")]
    Http2(h2::server::SendResponse<Bytes>),
}
/// A pipe to send a body after the [`Response`] is sent by
/// [`ResponsePipe::send_response`].
///
/// The [`AsyncWriteExt::shutdown`] does nothing, and will immediately return with Ok(())
#[derive(Debug)]
pub enum ResponseBodyPipe {
    /// HTTP/1 pipe
    Http1(Arc<Mutex<Encryption>>),
    /// HTTP/2 pipe
    #[cfg(feature = "h2")]
    Http2(h2::SendStream<Bytes>),
}
/// A [`ResponsePipe`]-like for a pushed request-response pair.
///
/// The only logic difference between this and [`ResponsePipe`] is the
/// lack of a `push_request` method. If you want to push more than one request,
/// use the same method on [`ResponsePipe`] more times.
#[derive(Debug)]
#[must_use]
pub enum PushedResponsePipe {
    /// A HTTP/2 pushed response pipe.
    ///
    /// This is the only variant for now, but as HTTP/3
    /// is implemented, a `Http3` variant will be added.
    #[cfg(feature = "h2")]
    Http2(h2::server::SendPushedResponse<Bytes>),
}

impl HttpConnection {
    /// Creates a new [`HttpConnection`] from an [`Encryption`] stream.
    ///
    /// # Errors
    ///
    /// Returns [`Error::VersionNotSupported`] when a unsupported version is passed.
    ///
    /// Also passes errors from [`h2::server::handshake`].
    pub async fn new(stream: Encryption, version: Version) -> Result<Self, Error> {
        match version {
            Version::HTTP_09 | Version::HTTP_10 | Version::HTTP_11 => {
                Ok(Self::Http1(Arc::new(Mutex::new(stream))))
            }
            #[cfg(feature = "h2")]
            Version::HTTP_2 => match h2::server::Builder::new()
                .max_concurrent_streams(250)
                .handshake(stream)
                .await
            {
                Ok(connection) => Ok(HttpConnection::Http2(Box::new(connection))),
                Err(err) => Err(Error::H2(err)),
            },
            #[cfg(not(feature = "h2"))]
            Version::HTTP_2 => Err(Error::VersionNotSupported),
            _ => Err(Error::VersionNotSupported),
        }
    }

    /// Accept a single request.
    /// `default_host` will be used if the `Host` header is not
    /// present on a HTTP/1.x request.
    ///
    /// # Errors
    ///
    /// Returns any errors emitted from [`h2::server::Connection::accept()`].
    pub async fn accept(
        &mut self,
        default_host: &[u8],
    ) -> Result<(Request<Body>, ResponsePipe), Error> {
        match self {
            Self::Http1(stream) => {
                let response = ResponsePipe::Http1(Arc::clone(stream));
                request::parse_http_1(Arc::clone(stream), 16 * 1024, default_host)
                    .await
                    .map(|request| (request, response))
            }
            #[cfg(feature = "h2")]
            Self::Http2(connection) => match connection.accept().await {
                Some(connection) => match connection {
                    Ok((request, response)) => {
                        Ok((request.map(Body::Http2), ResponsePipe::Http2(response)))
                    }
                    Err(err) => Err(Error::H2(err)),
                },
                None => Err(parse::Error::Done.into()),
            },
        }
    }
}

mod request {
    use super::{
        io, parse, response, utility, Arc, AsyncRead, Body, Bytes, Context, Encryption, Error,
        Mutex, Pin, Poll, ReadBuf, Request,
    };

    #[inline]
    pub(crate) async fn parse_http_1(
        stream: Arc<Mutex<Encryption>>,
        max_len: usize,
        default_host: &[u8],
    ) -> Result<Request<Body>, Error> {
        let scheme = match &*stream.lock().await {
            Encryption::Tcp(_) => "http",
            #[cfg(feature = "https")]
            Encryption::TcpTls(_) => "https",
        };
        let lock = stream.lock().await;

        let (head, bytes) = parse::request(lock, max_len, default_host, scheme).await?;
        let body = Body::Http1(response::Http1Body::new(
            stream,
            bytes,
            utility::get_body_length_request(&head),
        ));
        Ok(head.map(|()| body))
    }

    impl Body {
        /// Reads all bytes from [`Body`] to a [`Bytes`].
        ///
        /// # Errors
        ///
        /// Passes any errors returned from the inner reader.
        /// See [`super::Http1Body::read_to_bytes()`] and [`h2::RecvStream::poll_data()`] for more info.
        #[inline]
        pub async fn read_to_bytes(&mut self) -> io::Result<Bytes> {
            match self {
                Self::Empty => Ok(Bytes::new()),
                Self::Http1(h1) => h1.read_to_bytes().await,
                #[cfg(feature = "h2")]
                Self::Http2(h2) => futures::future::poll_fn(|cx| h2.poll_data(cx))
                    .await
                    .unwrap_or_else(|| Ok(Bytes::new()))
                    .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
            }
        }
    }

    impl AsyncRead for Body {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Http1(s) => unsafe { Pin::new_unchecked(s).poll_read(cx, buf) },
                #[cfg(feature = "h2")]
                Self::Http2(tls) => {
                    let data = match tls.poll_data(cx) {
                        Poll::Ready(data) => data,
                        Poll::Pending => return Poll::Pending,
                    };
                    match data {
                        Some(d) => match d {
                            Ok(data) => buf.put_slice(&data),
                            Err(err) => {
                                let err = io::Error::new(io::ErrorKind::InvalidData, err);
                                return Poll::Ready(Err(err));
                            }
                        },
                        None => return Poll::Ready(Ok(())),
                    }
                    Poll::Ready(Ok(()))
                }
                Self::Empty => Poll::Ready(Ok(())),
            }
        }
    }
}

mod response {
    use crate::prelude::{application::*, internals::*, *};

    /// A HTTP/1 body.
    ///
    /// The reason of this type and the inner buffer is described in [`super::Body::Http1`]
    #[must_use]
    pub struct Http1Body<R: AsyncRead + Unpin> {
        reader: Arc<Mutex<R>>,
        bytes: Bytes,
        offset: usize,

        content_length: usize,
    }
    impl<R: AsyncRead + Unpin> Http1Body<R> {
        /// Creates a new body.
        ///
        /// `content_length` should be the total length of the body, found in the [`Request::headers`].
        #[inline]
        pub fn new(reader: Arc<Mutex<R>>, bytes: Bytes, content_length: usize) -> Self {
            Self {
                reader,
                bytes,
                offset: 0,

                content_length,
            }
        }
        /// Reads all bytes from `self` to a [`Bytes`].
        ///
        /// # Errors
        ///
        /// Returns any errors from the underlying reader.
        #[inline]
        pub async fn read_to_bytes(&mut self) -> io::Result<Bytes> {
            let mut buffer = BytesMut::with_capacity(self.bytes.len() + 512);
            buffer.extend(&self.bytes);
            let len = self.content_length;
            if let Ok(result) = timeout(
                Duration::from_millis(250),
                utility::read_to_end_or_max(&mut buffer, &mut *self, len),
            )
            .await
            {
                result?
            }
            Ok(buffer.freeze())
        }
    }
    impl<R: AsyncRead + Unpin + Debug> Debug for Http1Body<R> {
        fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
            f.debug_struct("Http1Body")
                .field("reader", &self.reader)
                .field("buffer", &"[internal buffer]".as_clean())
                .field("offset", &self.offset)
                .field("content_length", &self.content_length)
                .finish()
        }
    }
    impl<R: AsyncRead + Unpin> AsyncRead for Http1Body<R> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            if self.offset < self.bytes.len() {
                let remaining = buf.remaining();
                if self.bytes.len() - self.offset > remaining {
                    buf.put_slice(&self.bytes[self.offset..self.offset + remaining]);
                    self.offset += remaining;
                } else {
                    buf.put_slice(&self.bytes[self.offset..]);
                    self.offset = self.bytes.len();
                }
                Poll::Pending
            } else {
                let mut reader = match self.reader.try_lock() {
                    Err(_) => return Poll::Pending,
                    Ok(r) => r,
                };
                let size = buf.filled().len();
                let result = unsafe { Pin::new_unchecked(&mut *reader).poll_read(cx, buf) };
                drop(reader);
                let difference = buf.filled().len() - size;
                self.offset += difference;
                if self.offset == self.content_length {
                    return Poll::Ready(Ok(()));
                }
                result
            }
        }
    }

    impl ResponsePipe {
        /// You must ensure the [`Response::version()`] is correct before calling this function.
        /// It can be guaranteed by first calling [`Self::ensure_version_and_length()`].
        ///
        /// It is critical to [`ResponseBodyPipe::close()`], else the message won't be seen as fully transmitted.
        ///
        /// # Errors
        ///
        /// Passes any errors from writing to the stream. see [`AsyncWriteExt::write()`] and
        /// [`h2::server::SendResponse::send_response()`] for more info.
        #[inline]
        pub async fn send_response(
            &mut self,
            mut response: Response<()>,
            #[allow(unused_variables)] end_of_stream: bool,
        ) -> Result<ResponseBodyPipe, Error> {
            match self {
                Self::Http1(s) => {
                    let mut writer = s.lock().await;
                    match response
                        .headers()
                        .get("connection")
                        .map(HeaderValue::to_str)
                        .and_then(Result::ok)
                    {
                        Some("close") | None => utility::replace_header_static(
                            response.headers_mut(),
                            "connection",
                            "keep-alive",
                        ),
                        _ => {}
                    }
                    let mut writer = tokio::io::BufWriter::with_capacity(512, &mut *writer);
                    utility::write::response(&response, b"", &mut writer)
                        .await
                        .map_err(Error::Io)?;
                    writer.flush().await.map_err(Error::Io)?;
                    writer.into_inner();

                    Ok(ResponseBodyPipe::Http1(Arc::clone(s)))
                }
                #[cfg(feature = "h2")]
                Self::Http2(s) => match s.send_response(response, end_of_stream) {
                    Err(err) => Err(Error::H2(err)),
                    Ok(pipe) => Ok(ResponseBodyPipe::Http2(pipe)),
                },
            }
        }
        /// Pushes `request` to client.
        ///
        /// # Errors
        ///
        /// If you try to push if `self` is [`ResponsePipe::Http1`], an [`Error::PushOnHttp1`] is returned.
        /// Returns errors from [`h2::server::SendResponse::push_request()`].
        #[inline]
        pub fn push_request(
            &mut self,
            #[allow(unused_variables)] request: Request<()>,
        ) -> Result<PushedResponsePipe, Error> {
            let mut request = request;
            utility::remove_all_headers(request.headers_mut(), "accept-encoding");
            match self {
                Self::Http1(_) => Err(Error::PushOnHttp1),
                #[cfg(feature = "h2")]
                Self::Http2(h2) => match h2.push_request(request) {
                    Ok(pipe) => Ok(PushedResponsePipe::Http2(pipe)),
                    Err(err) => Err(Error::H2(err)),
                },
            }
        }
        /// Ensures the version and length of the `response` using the variant of [`ResponsePipe`].
        #[inline]
        pub fn ensure_version_and_length<T>(&self, response: &mut Response<T>, len: usize) {
            utility::set_content_length(response.headers_mut(), len);
            utility::remove_all_headers(response.headers_mut(), "cache-control");
            // utility::remove_all_headers(response.headers_mut(), "content-encoding");
            utility::remove_all_headers(response.headers_mut(), "referrer-policy");
            utility::remove_all_headers(response.headers_mut(), "content-encoding");
            utility::remove_all_headers(response.headers_mut(), "server");
            match self {
                Self::Http1(_) => match response.version() {
                    Version::HTTP_09 | Version::HTTP_10 | Version::HTTP_11 => {
                        utility::set_content_length(response.headers_mut(), len);
                    }

                    _ => *response.version_mut() = Version::HTTP_11,
                },
                #[cfg(feature = "h2")]
                Self::Http2(_) => *response.version_mut() = Version::HTTP_2,
            }
        }
    }
    #[allow(unused_variables)]
    impl PushedResponsePipe {
        /// Sends a single push response.
        ///
        /// # Errors
        ///
        /// Errors are passed from the HTTP libraries, for now only [`mod@h2`].
        /// See [`h2::server::SendPushedResponse::send_response()`] for more information.
        #[inline]
        pub fn send_response(
            &mut self,
            response: Response<()>,
            end_of_stream: bool,
        ) -> Result<ResponseBodyPipe, Error> {
            match self {
                #[cfg(feature = "h2")]
                Self::Http2(s) => {
                    let mut response = response;
                    *response.version_mut() = Version::HTTP_2;

                    utility::remove_all_headers(response.headers_mut(), "accept-encoding");

                    match s.send_response(response, end_of_stream) {
                        Err(err) => Err(Error::H2(err)),
                        Ok(pipe) => Ok(ResponseBodyPipe::Http2(pipe)),
                    }
                }
                #[cfg(not(any(feature = "h2")))]
                _ => unreachable!(),
            }
        }
        /// Ensures the version of `response` depending on inner version if [`PushedResponsePipe`].
        #[inline]
        #[allow(unused_variables)]
        pub fn ensure_version_and_length<T>(&self, response: &mut Response<T>, len: usize) {
            utility::set_content_length(response.headers_mut(), len);
            utility::remove_all_headers(response.headers_mut(), "cache-control");
            // utility::remove_all_headers(response.headers_mut(), "content-encoding");
            utility::remove_all_headers(response.headers_mut(), "referrer-policy");
            utility::remove_all_headers(response.headers_mut(), "content-encoding");
            utility::remove_all_headers(response.headers_mut(), "server");
            match self {
                #[cfg(feature = "h2")]
                Self::Http2(_) => *response.version_mut() = Version::HTTP_2,
                #[cfg(not(any(feature = "h2")))]
                _ => unreachable!(),
            }
        }
    }

    impl ResponseBodyPipe {
        /// Sends `data` as the body.
        ///
        /// # Errors
        ///
        /// Passes any errors from writing to the stream.
        /// See [`AsyncWriteExt::write_all()`] and [`h2::SendStream::send_data()`].
        #[inline]
        pub async fn send(&mut self, data: Bytes) -> Result<(), Error> {
            self.send_with_maybe_close(data, false).await
        }
        /// Same as [`Self::send`] but with a `end_of_stream` variable.
        #[inline]
        pub(crate) async fn send_with_maybe_close(
            &mut self,
            data: Bytes,
            end_of_stream: bool,
        ) -> Result<(), Error> {
            match self {
                Self::Http1(h1) => {
                    let mut lock = h1.lock().await;
                    lock.write_all(&data).await?;
                    if end_of_stream {
                        lock.flush().await?;
                    }
                }
                #[cfg(feature = "h2")]
                Self::Http2(h2) => h2.send_data(data, end_of_stream)?,
            }
            Ok(())
        }
        /// Closes the pipe.
        ///
        /// # Errors
        ///
        /// Passes any errors emitted when closing the writer.
        /// See [`AsyncWriteExt::flush()`] and [`h2::SendStream::send_data()`].
        #[inline]
        pub async fn close(&mut self) -> Result<(), Error> {
            match self {
                Self::Http1(h1) => h1.lock().await.flush().await.map_err(Error::from),
                #[cfg(feature = "h2")]
                Self::Http2(h2) => h2.send_data(Bytes::new(), true).map_err(Error::from),
            }
        }
    }
    impl AsyncRead for ResponseBodyPipe {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Http1(s) => match s.try_lock() {
                    Err(_) => Poll::Pending,
                    Ok(mut s) => Pin::new(&mut *s).poll_read(cx, buf),
                },
                #[cfg(feature = "http2")]
                Self::Http2(_) => Poll::Ready(Ok(())),
            }
        }
    }
    impl AsyncWrite for ResponseBodyPipe {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<Result<usize, io::Error>> {
            match self.get_mut() {
                Self::Http1(s) => match s.try_lock() {
                    Err(_) => Poll::Pending,
                    Ok(mut s) => Pin::new(&mut *s).poll_write(cx, buf),
                },
                #[cfg(feature = "http2")]
                Self::Http2(s) => Poll::Ready(
                    s.send_data(Bytes::copy_from_slice(buf), false)
                        .map_err(|e| {
                            if e.is_io() {
                                // This is ok; we just checked it is IO.
                                e.into_io().unwrap()
                            } else {
                                io::Error::new(io::ErrorKind::Other, e.to_string())
                            }
                        })
                        .map(|()| buf.len()),
                ),
            }
        }
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            Poll::Ready(Ok(()))
        }
        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
            if let Self::Http1(s) = self.get_mut() {
                if let Ok(mut s) = s.try_lock() {
                    Pin::new(&mut *s).poll_flush(cx)
                } else {
                    Poll::Pending
                }
            } else {
                Poll::Ready(Ok(()))
            }
        }
    }
}
