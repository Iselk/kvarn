use kvarn::prelude::{internals::*, *};
use std::net::{Ipv4Addr, SocketAddrV4};
use tokio::net::{TcpStream, UdpSocket, UnixStream};

pub use async_bits::{poll_fn, CopyBuffer};
#[macro_use]
pub mod async_bits {
    use kvarn::prelude::*;
    macro_rules! ready {
        ($poll: expr) => {
            match $poll {
                Poll::Ready(v) => v,
                Poll::Pending => return Poll::Pending,
            }
        };
    }
    macro_rules! ret_ready_err {
        ($poll: expr) => {
            match $poll {
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(r) => Poll::Ready(r),
                _ => $poll,
            }
        };
        ($poll: expr, $map: expr) => {
            match $poll {
                Poll::Ready(Err(e)) => return Poll::Ready(Err($map(e))),
                Poll::Ready(r) => Poll::Ready(r),
                _ => Poll::Pending,
            }
        };
    }

    #[derive(Debug)]
    pub struct CopyBuffer {
        read_done: bool,
        pos: usize,
        cap: usize,
        buf: Box<[u8]>,
    }

    impl CopyBuffer {
        pub fn new() -> Self {
            Self {
                read_done: false,
                pos: 0,
                cap: 0,
                buf: std::vec::from_elem(0, 2048).into_boxed_slice(),
            }
        }
        pub fn with_capacity(initialized: usize) -> Self {
            Self {
                read_done: false,
                pos: 0,
                cap: 0,
                buf: std::vec::from_elem(0, initialized).into_boxed_slice(),
            }
        }

        /// Returns Ok(true) if it's done reading.
        pub fn poll_copy<R, W>(
            &mut self,
            cx: &mut Context<'_>,
            mut reader: Pin<&mut R>,
            mut writer: Pin<&mut W>,
        ) -> Poll<io::Result<bool>>
        where
            R: AsyncRead + ?Sized,
            W: AsyncWrite + ?Sized,
        {
            loop {
                // If our buffer is empty, then we need to read some data to
                // continue.
                if self.pos == self.cap && !self.read_done {
                    let me = &mut *self;
                    let mut buf = ReadBuf::new(&mut me.buf);
                    ready!(reader.as_mut().poll_read(cx, &mut buf))?;
                    let n = buf.filled().len();
                    if n == 0 {
                        self.read_done = true;
                    } else {
                        self.pos = 0;
                        self.cap = n;
                    }
                }

                // If our buffer has some data, let's write it out!
                while self.pos < self.cap {
                    let i = ready!(writer
                        .as_mut()
                        .poll_write(cx, &self.buf[self.pos..self.cap]))?;
                    if i == 0 {
                        return Poll::Ready(Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "write zero byte into writer",
                        )));
                    } else {
                        self.pos += i;
                    }
                    if self.pos >= self.cap {
                        return Poll::Ready(Ok(false));
                    }
                }

                // If we've written all the data and we've seen EOF, flush out the
                // data and finish the transfer.
                if self.pos == self.cap && self.read_done {
                    ready!(writer.as_mut().poll_flush(cx))?;
                    return Poll::Ready(Ok(true));
                }
            }
        }
    }
    impl Default for CopyBuffer {
        fn default() -> Self {
            Self::new()
        }
    }
    pub fn poll_fn<T, F>(f: F) -> PollFn<F>
    where
        F: FnMut(&mut Context<'_>) -> Poll<T>,
    {
        PollFn { f }
    }
    pub struct PollFn<F> {
        f: F,
    }
    impl<F> Unpin for PollFn<F> {}
    impl<F> fmt::Debug for PollFn<F> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("PollFn").finish()
        }
    }
    impl<T, F> Future for PollFn<F>
    where
        F: FnMut(&mut Context<'_>) -> Poll<T>,
    {
        type Output = T;

        fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
            (&mut self.f)(cx)
        }
    }
}

macro_rules! socket_addr_with_port {
        ($($port:literal $(,)+)*) => {
            &[
                $(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, $port)),)*
            ]
        };
    }

#[derive(Debug, Clone, Copy)]
pub enum Connection {
    Tcp(SocketAddr),
    /// Keep in mind, this currently has a `60s` timeout.
    /// Please use [`Self::UnixSocket`]s instead if you are on Unix.
    Udp(SocketAddr),
    #[cfg(unix)]
    UnixSocket(&'static Path),
}
impl Connection {
    pub async fn establish(self) -> io::Result<EstablishedConnection> {
        match self {
            Self::Tcp(addr) => TcpStream::connect(addr)
                .await
                .map(EstablishedConnection::Tcp),
            Self::Udp(addr) => {
                let candidates = &socket_addr_with_port!(
                    17448, 64567, 40022, 56654, 52027, 44328, 29973, 27919, 26513, 42327, 64855,
                    5296, 52942, 43204, 15322, 13243,
                )[..];
                let socket = UdpSocket::bind(candidates).await?;
                socket.connect(addr).await?;
                Ok(EstablishedConnection::Udp(socket))
            }
            Self::UnixSocket(path) => UnixStream::connect(path)
                .await
                .map(EstablishedConnection::UnixSocket),
        }
    }
}
#[derive(Debug)]
pub enum GatewayError {
    Io(io::Error),
    Timeout,
    Parse(parse::Error),
}
impl From<io::Error> for GatewayError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}
impl From<parse::Error> for GatewayError {
    fn from(err: parse::Error) -> Self {
        Self::Parse(err)
    }
}
#[derive(Debug)]
pub enum EstablishedConnection {
    Tcp(TcpStream),
    Udp(UdpSocket),
    #[cfg(unix)]
    UnixSocket(UnixStream),
}
impl AsyncWrite for EstablishedConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_write(cx, buf),
            Self::Udp(s) => Pin::new(s).poll_send(cx, buf),
            Self::UnixSocket(s) => Pin::new(s).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_flush(cx),
            Self::Udp(_) => Poll::Ready(Ok(())),
            Self::UnixSocket(s) => Pin::new(s).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_shutdown(cx),
            Self::Udp(_) => Poll::Ready(Ok(())),
            Self::UnixSocket(s) => Pin::new(s).poll_shutdown(cx),
        }
    }
}
impl AsyncRead for EstablishedConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(s) => Pin::new(s).poll_read(cx, buf),
            Self::Udp(s) => Pin::new(s).poll_recv(cx, buf),
            Self::UnixSocket(s) => Pin::new(s).poll_read(cx, buf),
        }
    }
}
impl EstablishedConnection {
    pub async fn request<T: Debug>(
        &mut self,
        request: &Request<T>,
        body: &[u8],
    ) -> Result<Response<Bytes>, GatewayError> {
        pub fn read_to_end(buffer: &mut BytesMut, mut reader: impl Read) -> io::Result<()> {
            let mut read = buffer.len();
            // This is safe because of the trailing unsafe block.
            unsafe { buffer.set_len(buffer.capacity()) };
            loop {
                match reader.read(&mut buffer[read..])? {
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

        let mut buffered = tokio::io::BufWriter::new(&mut *self);
        write::request(request, body, &mut buffered).await?;

        debug!("Sent reverse-proxy request.");

        let response = match timeout(std::time::Duration::from_millis(1000), async {
            kvarn::prelude::async_bits::read::response(&mut *self, 16 * 1024).await
        })
        .await
        {
            Ok(result) => match result {
                Err(err) => return Err(err.into()),
                Ok(response) => {
                    enum MaybeChunked<R1, R2> {
                        No(R1),
                        Yes(async_chunked_transfer::Decoder<R2>),
                    }
                    impl<R1: AsyncRead + Unpin, R2: AsyncRead + Unpin> AsyncRead for MaybeChunked<R1, R2> {
                        fn poll_read(
                            mut self: Pin<&mut Self>,
                            cx: &mut Context<'_>,
                            buf: &mut ReadBuf<'_>,
                        ) -> Poll<io::Result<()>> {
                            match &mut *self {
                                Self::No(reader) => Pin::new(reader).poll_read(cx, buf),
                                Self::Yes(reader) => Pin::new(reader).poll_read(cx, buf),
                            }
                        }
                    }

                    let chunked = header_eq(response.headers(), "transfer-encoding", "chunked");
                    let len = if chunked {
                        usize::MAX
                    } else {
                        get_body_length_response(&response, Some(request.method()))
                    };

                    let (mut head, body) = split_response(response);

                    let body = if len == 0 || len <= body.len() {
                        body
                    } else {
                        let mut buffer = BytesMut::with_capacity(body.len() + 512);

                        let reader = if chunked {
                            let reader = AsyncReadExt::chain(&*body, &mut *self);
                            let decoder = async_chunked_transfer::Decoder::new(reader);
                            MaybeChunked::Yes(decoder)
                        } else {
                            buffer.extend(&body);
                            MaybeChunked::No(&mut *self)
                        };

                        if let Ok(result) = timeout(
                            tokio::time::Duration::from_millis(250),
                            read_to_end_or_max(&mut buffer, reader, len),
                        )
                        .await
                        {
                            result?
                        } else {
                            warn!("Remote read timed out.");
                            unsafe { buffer.set_len(0) };
                        }

                        if chunked {
                            remove_all_headers(head.headers_mut(), "transfer-encoding");
                            info!("Decoding chunked transfer-encoding.");
                        }
                        buffer.freeze()
                    };

                    head.map(|()| body)
                }
            },
            Err(_) => return Err(GatewayError::Timeout),
        };
        Ok(response)
    }
}

#[derive(Debug)]
pub enum OpenBackError {
    Front(io::Error),
    Back(io::Error),
    Closed,
}
impl OpenBackError {
    pub fn get_io(&self) -> Option<&io::Error> {
        match self {
            Self::Front(e) | Self::Back(e) => Some(e),
            Self::Closed => None,
        }
    }
    pub fn get_io_kind(&self) -> io::ErrorKind {
        match self {
            Self::Front(e) | Self::Back(e) => e.kind(),
            Self::Closed => io::ErrorKind::BrokenPipe,
        }
    }
}
pub struct ByteProxy<'a, F: AsyncRead + AsyncWrite + Unpin, B: AsyncRead + AsyncWrite + Unpin> {
    front: &'a mut F,
    back: &'a mut B,
    // ToDo: Optimize to one buffer!
    front_buf: CopyBuffer,
    back_buf: CopyBuffer,
}
impl<'a, F: AsyncRead + AsyncWrite + Unpin, B: AsyncRead + AsyncWrite + Unpin> ByteProxy<'a, F, B> {
    pub fn new(front: &'a mut F, back: &'a mut B) -> Self {
        Self {
            front,
            back,
            front_buf: CopyBuffer::new(),
            back_buf: CopyBuffer::new(),
        }
    }
    pub fn poll_channel(&mut self, cx: &mut Context) -> Poll<Result<(), OpenBackError>> {
        macro_rules! copy_from_to {
            ($reader: expr, $error: expr, $buf: expr, $writer: expr) => {
                if let Poll::Ready(Ok(pipe_closed)) = ret_ready_err!(
                    $buf.poll_copy(cx, Pin::new($reader), Pin::new($writer)),
                    $error
                ) {
                    if pipe_closed {
                        return Poll::Ready(Err(OpenBackError::Closed));
                    } else {
                        return Poll::Ready(Ok(()));
                    }
                };
            };
        }

        copy_from_to!(self.back, OpenBackError::Back, self.front_buf, self.front);
        copy_from_to!(self.front, OpenBackError::Front, self.back_buf, self.back);

        Poll::Pending
    }
    pub async fn channel(&mut self) -> Result<(), OpenBackError> {
        poll_fn(|cx| self.poll_channel(cx)).await
    }
}

pub type ModifyRequestFn = Arc<dyn Fn(&mut Request<()>, &mut Bytes) + Send + Sync>;
pub type GetConnectionFn = Arc<dyn (Fn(&FatRequest, &Bytes) -> Option<Connection>) + Send + Sync>;

/// Creates a new [`GetConnectionFn`] which always returns `kind`
pub fn static_connection(kind: Connection) -> GetConnectionFn {
    Arc::new(move |_, _| Some(kind))
}

pub struct Manager {
    when: extensions::If,
    connection: GetConnectionFn,
    modify: ModifyRequestFn,
}
impl Manager {
    /// Consider using [`static_connection`] if your connection type is not dependent of the request.
    pub fn new(when: extensions::If, connection: GetConnectionFn, modify: ModifyRequestFn) -> Self {
        Self {
            when,
            connection,
            modify,
        }
    }
    /// Consider using [`static_connection`] if your connection type is not dependent of the request.
    pub fn base(base_path: &str, connection: GetConnectionFn) -> Self {
        assert_eq!(base_path.chars().next(), Some('/'));
        let path = if base_path.ends_with('/') {
            base_path.to_owned()
        } else {
            let mut s = String::with_capacity(base_path.len() + 1);
            s.push_str(base_path);
            s.push('/');
            s
        };
        let path = Arc::new(path);

        let when_path = Arc::clone(&path);
        let when = Box::new(move |request: &FatRequest, _host: &Host| {
            request.uri().path().starts_with(when_path.as_str())
        });

        let modify: ModifyRequestFn = Arc::new(move |request, _| {
            let path = Arc::clone(&path);

            let mut parts = request.uri().clone().into_parts();

            if let Some(path_and_query) = parts.path_and_query.as_ref() {
                if let Some(s) = path_and_query.as_str().get(path.as_str().len() - 1..) {
                    // We know this is a good path and query; we've just removed the first x bytes.
                    // The -1 will always be on a char boundary; the last character is always '/'
                    let short =
                        uri::PathAndQuery::from_maybe_shared(Bytes::copy_from_slice(s.as_bytes()))
                            .unwrap();
                    parts.path_and_query = Some(short);
                    parts.scheme = Some(uri::Scheme::HTTP);
                    // For unwrap, see ↑
                    let uri = Uri::from_parts(parts).unwrap();
                    *request.uri_mut() = uri;
                } else {
                    error!("We didn't get the expected path string from Kvarn. We asked for one which started with `base_path`");
                }
            }
        });

        Self {
            when,
            connection,
            modify,
        }
    }
    pub fn mount(self, extensions: &mut Extensions) {
        let connection = self.connection;
        let modify = self.modify;

        macro_rules! return_status {
            ($result:expr, $status:expr, $host:expr) => {
                match $result {
                    Some(v) => v,
                    None => {
                        return default_error_response($status, $host, None).await;
                    }
                }
            };
        }

        extensions.add_prepare_fn(
            self.when,
            prepare!(req, host, _path, _addr, move |connection, modify| {
                let mut empty_req = empty_clone_request(&req);
                let mut bytes = return_status!(
                    req.body_mut().read_to_bytes().await.ok(),
                    StatusCode::BAD_GATEWAY,
                    host
                );

                let connection =
                    return_status!(connection(req, &bytes), StatusCode::BAD_REQUEST, host);
                let mut connection = return_status!(
                    connection.establish().await.ok(),
                    StatusCode::GATEWAY_TIMEOUT,
                    host
                );

                replace_header_static(empty_req.headers_mut(), "accept-encoding", "identity");

                if header_eq(empty_req.headers(), "connection", "keep-alive") {
                    replace_header_static(empty_req.headers_mut(), "connection", "close");
                }

                *empty_req.version_mut() = Version::HTTP_11;

                let wait = matches!(empty_req.method(), &Method::CONNECT)
                    || empty_req.headers().get("upgrade")
                        == Some(&HeaderValue::from_static("websocket"));

                modify(&mut empty_req, &mut bytes);

                let mut response = match connection.request(&empty_req, &bytes).await {
                    Ok(mut response) => {
                        let headers = response.headers_mut();
                        remove_all_headers(headers, "keep-alive");
                        if !header_eq(headers, "connection", "upgrade") {
                            remove_all_headers(headers, "connection");
                        }

                        FatResponse::cache(response)
                    }
                    Err(err) => {
                        warn!("Got error {:?}", err);
                        default_error_response(
                            match err {
                                GatewayError::Io(_) | GatewayError::Parse(_) => {
                                    StatusCode::BAD_GATEWAY
                                }
                                GatewayError::Timeout => StatusCode::GATEWAY_TIMEOUT,
                            },
                            host,
                            None,
                        )
                        .await
                    }
                };

                if wait {
                    info!("Keeping the pipe open!");
                    let future = response_pipe_fut!(response_pipe, _host {
                        let udp_connection = matches!(connection, EstablishedConnection::Udp(_));

                        let mut open_back = ByteProxy::new(response_pipe, &mut connection);
                        debug!("Created open back!");

                        loop {
                            // Add 60 second timeout to UDP connections.
                            let timeout_result = if udp_connection {
                                timeout(std::time::Duration::from_secs(90), open_back.channel())
                                .await
                            }else {
                                Ok(open_back.channel().await)
                            };

                            if let Ok(r) = timeout_result
                            {
                                debug!("Open back responded! {:?}", r);
                                match r {
                                    Err(err) => {
                                        if !matches!(
                                            err.get_io_kind(),
                                            io::ErrorKind::ConnectionAborted
                                                | io::ErrorKind::ConnectionReset
                                                | io::ErrorKind::BrokenPipe
                                        ) {
                                            warn!("Reverse proxy io error: {:?}", err);
                                        }
                                        break;
                                    },
                                    Ok(()) => continue,
                                }
                            } else {
                                break;
                            }
                        }
                    });

                    response = response
                        .with_future(future)
                        .with_compress(CompressPreference::None);
                }

                response
            }),
            extensions::Id::new(-128, "Reverse proxy").no_override(),
        );
    }
}

pub fn localhost(port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}
