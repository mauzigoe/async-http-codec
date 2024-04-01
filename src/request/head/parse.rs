use http::header::HeaderName;
use http::request::Parts;
use http::{HeaderValue, Method, Request, Uri, Version};
use std::io;
use std::io::ErrorKind::InvalidData;
use std::io::Read;

struct TerminatorOverlap<'a> {
    terminator: &'a[u8],
    overlap: usize
}

impl<'a> TerminatorOverlap<'a> {
    fn new(terminator: &'a[u8]) -> TerminatorOverlap {
	TerminatorOverlap {
	    terminator,
	    overlap: 0
	}
    }
    /// returns min number of unprocessed bytes (remaining unmatched terminator bytes)
    fn remaining(&self) -> usize {
	self.terminator.len() - self.overlap
    }
    /// scans data for overlap with remaining terminator bytes
    fn process(&mut self, data: &[u8]) {
        match self.overlap {
            0 => {
                for i in 0..data.len() {
                    let window_size = self.terminator.len() - i;
                    if data[i..] == self.terminator[0..window_size] {
                        self.overlap = window_size;
                    }
                }
            },
            x => {
                match data[0..(self.terminator.len() - x)] == self.terminator[x..] {
                    true => self.overlap = self.terminator.len(),
                    false => self.overlap = 0,
                }
            }
        }
    }
    /// true if complete terminator was processed
    fn done(&self) -> bool {
	self.overlap == self.terminator.len()
    }
    /// slice read buffer to maximum size guaranteed to not read past the terminator
    fn max_read_buf(&self, buf: &'a mut [u8]) -> &'a mut [u8] {
	let len = buf.len();
	&mut buf[0..len.min(self.remaining())]
    }
}

pub struct RequestHeadParse<'a> {
    buffer: Vec<u8>,
    terminator: TerminatorOverlap<'a>,
    max_headers: usize,
}

impl<'a> RequestHeadParse<'a> {
    const END: &'a [u8] = b"\r\n\r\n";
    pub fn new(max_buffer: usize, max_headers: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(max_buffer),
            terminator: TerminatorOverlap::new(Self::END),
            max_headers,
        }
    }
    pub fn read_data<T: Read>(&mut self, rd: &mut T) -> Result<usize, std::io::Error> {
        let mut chunks = [0u8; Self::END.len()];
	while !self.terminator.done() {
	    let chunks = self.terminator.max_read_buf(&mut chunks);
	    if self.buffer.capacity() - self.buffer.len() < chunks.len() {
		return Err(std::io::ErrorKind::OutOfMemory.into());
	    }
	    rd.read_exact(chunks)?;
	    self.terminator.process(&chunks);
	    self.buffer.extend_from_slice(chunks);
	}
        Ok(self.buffer.len())
    }
    pub fn try_take_head(&mut self) -> io::Result<Parts> {
        let mut headers = vec![httparse::EMPTY_HEADER; self.max_headers];
        let mut parsed_request = httparse::Request::new(&mut headers);
        if parsed_request
            .parse(self.buffer.as_ref())
            .map_err(|err| io::Error::new(InvalidData, err.to_string()))?
            .is_partial()
        {
            return Err(io::Error::new(InvalidData, "malformed HTTP head"));
        }
        if parsed_request.version != Some(1) {
            return Err(io::Error::new(InvalidData, "unsupported HTTP version"));
        }
        let method = Method::from_bytes(parsed_request.method.unwrap_or("").as_bytes())
            .map_err(|err| io::Error::new(InvalidData, err.to_string()))?;
        let uri = parsed_request
            .path
            .unwrap_or("")
            .parse::<Uri>()
            .map_err(|_| io::Error::new(InvalidData, "invalid uri"))?;
        let mut request = Request::new(());
        *request.method_mut() = method;
        *request.uri_mut() = uri;
        *request.version_mut() = Version::HTTP_11;
        let headers = request.headers_mut();
        headers.reserve(parsed_request.headers.len());
        for header in parsed_request.headers {
            headers.append(
                HeaderName::from_bytes(header.name.as_bytes())
                    .map_err(|_| io::Error::new(InvalidData, "invalid header name"))?,
                HeaderValue::from_bytes(header.value)
                    .map_err(|_| io::Error::new(InvalidData, "invalid header value"))?,
            );
        }
        Ok(request.into_parts().0)
    }
}
