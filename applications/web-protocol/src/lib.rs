//! # Hail — the CIBOS request/response protocol
//!
//! Hail is the application-layer protocol CIBOS uses over Lattice [`Link`]s — the
//! rough equivalent of HTTP, but minimal and message-framed: because a Link
//! delivers whole messages, one request is one message and one response is one
//! message, so there is no length-delimiting or chunking to parse.
//!
//! Wire format (UTF-8 head line, then `\n`, then a raw body):
//!
//! ```text
//! request:   FETCH /index.html\n<body>
//! response:  200 OK\n<body bytes>
//! ```
//!
//! [`Link`]: cibos_sdk
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fmt;

/// Request verbs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// Retrieve a resource (like HTTP GET).
    Fetch,
    /// Store a resource (like HTTP PUT).
    Push,
}

impl Verb {
    fn as_str(self) -> &'static str {
        match self {
            Verb::Fetch => "FETCH",
            Verb::Push => "PUSH",
        }
    }

    fn parse(s: &str) -> Option<Verb> {
        match s {
            "FETCH" => Some(Verb::Fetch),
            "PUSH" => Some(Verb::Push),
            _ => None,
        }
    }
}

/// Common Hail status codes.
pub mod status {
    /// Success.
    pub const OK: u16 = 200;
    /// Resource created/stored.
    pub const STORED: u16 = 201;
    /// Malformed request.
    pub const BAD_REQUEST: u16 = 400;
    /// Resource not found.
    pub const NOT_FOUND: u16 = 404;
    /// Server error.
    pub const SERVER_ERROR: u16 = 500;
}

/// A decoded request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The verb.
    pub verb: Verb,
    /// The resource path.
    pub path: String,
    /// The request body (empty for `Fetch`).
    pub body: Vec<u8>,
}

impl Request {
    /// A `Fetch` request for `path`.
    #[must_use]
    pub fn fetch(path: &str) -> Request {
        Request {
            verb: Verb::Fetch,
            path: path.to_string(),
            body: Vec::new(),
        }
    }

    /// A `Push` request storing `body` at `path`.
    #[must_use]
    pub fn push(path: &str, body: Vec<u8>) -> Request {
        Request {
            verb: Verb::Push,
            path: path.to_string(),
            body,
        }
    }
}

/// A decoded response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Status code.
    pub status: u16,
    /// Human-readable reason.
    pub reason: String,
    /// Response body.
    pub body: Vec<u8>,
}

impl Response {
    /// A 200 OK response with `body`.
    #[must_use]
    pub fn ok(body: Vec<u8>) -> Response {
        Response {
            status: status::OK,
            reason: "OK".to_string(),
            body,
        }
    }

    /// A response with the given status, a standard reason, and `body`.
    #[must_use]
    pub fn with_status(status: u16, body: Vec<u8>) -> Response {
        let reason = match status {
            status::OK => "OK",
            status::STORED => "STORED",
            status::BAD_REQUEST => "BAD-REQUEST",
            status::NOT_FOUND => "NOT-FOUND",
            status::SERVER_ERROR => "ERROR",
            _ => "STATUS",
        };
        Response {
            status,
            reason: reason.to_string(),
            body,
        }
    }
}

/// Errors decoding a Hail message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolError {
    /// The message had no head line / body separator or was empty.
    Malformed,
    /// The request verb was not recognized.
    UnknownVerb,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProtocolError::Malformed => write!(f, "malformed message"),
            ProtocolError::UnknownVerb => write!(f, "unknown verb"),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// Split a message into its head line (UTF-8) and body bytes at the first `\n`.
fn split_head(bytes: &[u8]) -> Result<(&str, &[u8]), ProtocolError> {
    let nl = bytes.iter().position(|&b| b == b'\n');
    let (head_bytes, body) = match nl {
        Some(i) => (&bytes[..i], &bytes[i + 1..]),
        None => (bytes, &[][..]), // head only, empty body
    };
    let head = std::str::from_utf8(head_bytes).map_err(|_| ProtocolError::Malformed)?;
    Ok((head, body))
}

/// Encode a request to wire bytes.
#[must_use]
pub fn encode_request(req: &Request) -> Vec<u8> {
    let mut out = format!("{} {}\n", req.verb.as_str(), req.path).into_bytes();
    out.extend_from_slice(&req.body);
    out
}

/// Decode a request from wire bytes.
///
/// # Errors
/// [`ProtocolError`] if the head line is missing, malformed, or names an
/// unknown verb.
pub fn decode_request(bytes: &[u8]) -> Result<Request, ProtocolError> {
    let (head, body) = split_head(bytes)?;
    let mut it = head.split_whitespace();
    let verb = Verb::parse(it.next().ok_or(ProtocolError::Malformed)?)
        .ok_or(ProtocolError::UnknownVerb)?;
    let path = it.next().ok_or(ProtocolError::Malformed)?.to_string();
    Ok(Request {
        verb,
        path,
        body: body.to_vec(),
    })
}

/// Encode a response to wire bytes.
#[must_use]
pub fn encode_response(resp: &Response) -> Vec<u8> {
    let mut out = format!("{} {}\n", resp.status, resp.reason).into_bytes();
    out.extend_from_slice(&resp.body);
    out
}

/// Decode a response from wire bytes.
///
/// # Errors
/// [`ProtocolError::Malformed`] if the head line is missing or the status code
/// does not parse.
pub fn decode_response(bytes: &[u8]) -> Result<Response, ProtocolError> {
    let (head, body) = split_head(bytes)?;
    let mut it = head.splitn(2, ' ');
    let status: u16 = it
        .next()
        .ok_or(ProtocolError::Malformed)?
        .parse()
        .map_err(|_| ProtocolError::Malformed)?;
    let reason = it.next().unwrap_or("").to_string();
    Ok(Response {
        status,
        reason,
        body: body.to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trip() {
        let req = Request::fetch("/index.html");
        let wire = encode_request(&req);
        assert_eq!(decode_request(&wire).unwrap(), req);

        let req = Request::push("/notes.txt", b"hello".to_vec());
        let wire = encode_request(&req);
        let back = decode_request(&wire).unwrap();
        assert_eq!(back.verb, Verb::Push);
        assert_eq!(back.path, "/notes.txt");
        assert_eq!(back.body, b"hello");
    }

    #[test]
    fn response_round_trip_with_binary_body() {
        let resp = Response::ok(vec![0u8, 1, 2, 255, b'\n', b'x']);
        let wire = encode_response(&resp);
        let back = decode_response(&wire).unwrap();
        assert_eq!(back.status, 200);
        assert_eq!(back.body, vec![0u8, 1, 2, 255, b'\n', b'x']);
    }

    #[test]
    fn unknown_verb_and_malformed() {
        assert_eq!(decode_request(b"DELETE /x\n").err(), Some(ProtocolError::UnknownVerb));
        assert_eq!(decode_request(b"FETCH\n").err(), Some(ProtocolError::Malformed));
        assert_eq!(decode_response(b"notanumber OK\n").err(), Some(ProtocolError::Malformed));
    }

    #[test]
    fn status_reasons() {
        assert_eq!(Response::with_status(status::NOT_FOUND, vec![]).reason, "NOT-FOUND");
        assert_eq!(Response::with_status(status::STORED, vec![]).reason, "STORED");
    }
}
