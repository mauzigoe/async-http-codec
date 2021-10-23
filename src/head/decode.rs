use anyhow::bail;
use futures_lite::prelude::*;
use http::header::HeaderName;
use http::{HeaderValue, Method, Request, Uri, Version};
use std::borrow::BorrowMut;
use std::marker::PhantomData;
use std::pin::Pin;
use std::task::{Context, Poll};

#[derive(Copy, Clone, Debug)]
pub struct RequestHeadDecoder {
    max_head_size: usize,
    max_headers: usize,
}

impl RequestHeadDecoder {
    pub fn decode<T: AsyncRead + Unpin, R: BorrowMut<T>>(
        self,
        transport: R,
    ) -> RequestHeadDecode<T, R> {
        RequestHeadDecode {
            buffer: Vec::with_capacity(self.max_head_size),
            transport: Some(transport),
            decoder: self,
            completion: 0,
            p: Default::default(),
        }
    }
    pub fn decode_ref<T: AsyncRead + Unpin>(
        self,
        transport: &mut T,
    ) -> RequestHeadDecode<T, &mut T> {
        self.decode(transport)
    }
}

impl Default for RequestHeadDecoder {
    fn default() -> Self {
        Self {
            max_head_size: 8192,
            max_headers: 128,
        }
    }
}

#[pin_project::pin_project]
pub struct RequestHeadDecode<T: AsyncRead + Unpin, R: BorrowMut<T>> {
    buffer: Vec<u8>,
    transport: Option<R>,
    decoder: RequestHeadDecoder,
    completion: usize,
    p: PhantomData<*const T>,
}

impl<T: AsyncRead + Unpin, R: BorrowMut<T>> Future for RequestHeadDecode<T, R> {
    type Output = anyhow::Result<(R, Request<()>)>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        assert_ne!(self.completion, usize::MAX);
        const END: &[u8; 4] = b"\r\n\r\n";
        let mut chunk = [0u8; 4];
        loop {
            let chunk = &mut chunk[self.completion..4];
            if self.buffer.len() + chunk.len() > self.buffer.capacity() {
                return Poll::Ready(Err(anyhow::Error::msg("request head too long")));
            }
            let transport = Pin::new(self.transport.as_mut().unwrap().borrow_mut());
            match transport.poll_read(cx, chunk) {
                Poll::Ready(Ok(n)) => {
                    let chunk = &chunk[0..n];
                    self.buffer.extend_from_slice(chunk);
                    match chunk == &END[self.completion..self.completion + n] {
                        true => self.completion += n,
                        false => self.completion = 0,
                    }
                    if self.completion == END.len() {
                        self.completion = usize::MAX;
                        return Poll::Ready(
                            request_head_parse(&self.buffer, self.decoder.max_headers)
                                .map(|request| (self.transport.take().unwrap(), request)),
                        );
                    }
                }
                Poll::Ready(Err(err)) => {
                    self.completion = usize::MAX;
                    return Poll::Ready(Err(err.into()));
                }
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

fn request_head_parse(buffer: &[u8], max_headers: usize) -> anyhow::Result<Request<()>> {
    let mut headers = vec![httparse::EMPTY_HEADER; max_headers];
    let mut parsed_request = httparse::Request::new(&mut headers);
    if parsed_request.parse(buffer)?.is_partial() {
        bail!("invalid HTTP head")
    }
    if parsed_request.version != Some(1) {
        bail!("unsupported HTTP version")
    }
    let method = Method::from_bytes(parsed_request.method.unwrap_or("").as_bytes())?;
    let uri = parsed_request.path.unwrap_or("").parse::<Uri>()?;
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .version(Version::HTTP_11)
        .body(())?;
    let headers = request.headers_mut();
    headers.reserve(parsed_request.headers.len());
    for header in parsed_request.headers {
        headers.append(
            HeaderName::from_bytes(header.name.as_bytes())?,
            HeaderValue::from_bytes(header.value)?,
        );
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use crate::head::decode::RequestHeadDecoder;
    use futures_lite::future::block_on;
    use futures_lite::io::Cursor;
    use futures_lite::{AsyncReadExt, StreamExt};
    use http::{Method, Request, Version};

    const INPUT: &[u8] = b"GET / HTTP/1.1\r\nHost: www.example.com\r\nConnection: close\r\n\r\n ";

    async fn check(output: Request<()>, transport: Cursor<&[u8]>) {
        assert_eq!(output.version(), Version::HTTP_11);
        assert_eq!(output.method(), Method::GET);
        assert_eq!(
            output.headers().get("Host").unwrap().as_bytes(),
            b"www.example.com"
        );
        assert_eq!(
            output.headers().get("Connection").unwrap().as_bytes(),
            b"close"
        );
        assert_eq!(transport.bytes().count().await, 1);
    }

    #[test]
    fn owned_transport() {
        block_on(async {
            let transport = Cursor::new(INPUT);
            let (transport, output) = RequestHeadDecoder::default()
                .decode(transport)
                .await
                .unwrap();
            check(output, transport).await;
        })
    }

    #[test]
    fn referenced_transport() {
        block_on(async {
            let mut transport = Cursor::new(INPUT);
            let (_, output) = RequestHeadDecoder::default()
                .decode_ref(&mut transport)
                .await
                .unwrap();
            check(output, transport).await;
        })
    }
}