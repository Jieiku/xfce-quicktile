//! Typed command and response framing for the daemon's Unix-socket protocol.

use anyhow::{bail, Context, Result};
use std::{io::Read, str::FromStr};

/// Hard cap on the daemon-side request size. A request is a single short
/// command word, so anything larger is malformed (or hostile) and is
/// rejected instead of being buffered unboundedly.
const MAX_REQUEST_BYTES: usize = 128;

/// A command the daemon can execute. This is the only request payload on
/// the wire: the client writes `as_str()` and the daemon parses it back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Left,
    Right,
    Up,
    Down,
    Ping,
    Stop,
}

impl Command {
    /// The wire name: the exact bytes the client sends (no trailing
    /// newline).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Left => "left",
            Self::Right => "right",
            Self::Up => "up",
            Self::Down => "down",
            Self::Ping => "ping",
            Self::Stop => "stop",
        }
    }
}

impl FromStr for Command {
    type Err = String;

    /// Parse a wire command name. Case-sensitive, matching the wire format.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "up" => Ok(Self::Up),
            "down" => Ok(Self::Down),
            "ping" => Ok(Self::Ping),
            "stop" => Ok(Self::Stop),
            // "direction", not "command": must stay byte-identical to the pre-refactor daemon wire text (`a8aea57:src/tiling.rs:39`), which raw-socket clients observe exactly.
            _ => Err(format!("unknown direction `{value}`")),
        }
    }
}

/// A framed reply the daemon writes and the client parses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// Wire form: `OK {message}\n`.
    Ok(String),
    /// Wire form: `ERR {message}\n`.
    Err(String),
}

impl Response {
    /// Serialize for the wire: `OK {message}\n` or `ERR {message}\n`.
    /// `&self`: the reply is small and cheap to borrow-serialize, and
    /// `Response` is not `Copy`.
    pub fn to_wire(&self) -> String {
        match self {
            Self::Ok(message) => format!("OK {message}\n"),
            Self::Err(message) => format!("ERR {message}\n"),
        }
    }

    /// Parse a daemon reply. `line` must already be trimmed; an unknown
    /// framing is an error, not a guess.
    pub fn from_wire(line: &str) -> Result<Self> {
        if let Some(message) = line.strip_prefix("OK ") {
            Ok(Self::Ok(message.to_string()))
        } else if let Some(message) = line.strip_prefix("ERR ") {
            Ok(Self::Err(message.to_string()))
        } else {
            bail!("unexpected daemon response: {line}")
        }
    }
}

/// Read one request on the daemon side of the socket: until EOF, because
/// the client shuts down the write side after sending the command name.
/// Bounded by [`MAX_REQUEST_BYTES`] — a peer sending more is rejected
/// rather than buffered unboundedly (fixed stack buffer + pre-sized `Vec`,
/// so no unbounded allocation is possible). Returns the trimmed request.
pub fn read_request(stream: &mut (impl Read + ?Sized)) -> Result<String> {
    // One byte past the cap so a single overlong read is detected and
    // rejected instead of silently truncated.
    let mut chunk = [0u8; MAX_REQUEST_BYTES + 1];
    let mut request = Vec::with_capacity(MAX_REQUEST_BYTES);
    loop {
        let read = stream.read(&mut chunk).context("reading socket request")?;
        if read == 0 {
            break;
        }
        if request.len() + read > MAX_REQUEST_BYTES {
            bail!("request exceeds {MAX_REQUEST_BYTES} bytes");
        }
        request.extend_from_slice(&chunk[..read]);
    }
    let request = String::from_utf8(request).context("reading socket request")?;
    Ok(request.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_parses_all_command_names() {
        assert_eq!(Command::from_str("left").unwrap(), Command::Left);
        assert_eq!(Command::from_str("right").unwrap(), Command::Right);
        assert_eq!(Command::from_str("up").unwrap(), Command::Up);
        assert_eq!(Command::from_str("down").unwrap(), Command::Down);
        assert_eq!(Command::from_str("ping").unwrap(), Command::Ping);
        assert_eq!(Command::from_str("stop").unwrap(), Command::Stop);
    }

    #[test]
    fn from_str_rejects_unknown_and_misnamed_values() {
        let err = Command::from_str("lefty").unwrap_err();
        assert_eq!(err, "unknown direction `lefty`");
        // Case-sensitive and non-empty, matching the wire format.
        assert!(Command::from_str("LEFT").is_err());
        assert!(Command::from_str("").is_err());
    }

    #[test]
    fn as_str_round_trips_through_from_str() {
        for command in [
            Command::Left,
            Command::Right,
            Command::Up,
            Command::Down,
            Command::Ping,
            Command::Stop,
        ] {
            assert_eq!(Command::from_str(command.as_str()).unwrap(), command);
        }
    }

    #[test]
    fn from_wire_parses_ok_and_err() {
        assert_eq!(
            Response::from_wire("OK something").unwrap(),
            Response::Ok("something".to_string())
        );
        assert_eq!(
            Response::from_wire("ERR something").unwrap(),
            Response::Err("something".to_string())
        );
    }

    #[test]
    fn from_wire_rejects_malformed_lines() {
        let err = Response::from_wire("nope").unwrap_err();
        assert_eq!(err.to_string(), "unexpected daemon response: nope");
    }

    #[test]
    fn to_wire_emits_exact_framing() {
        assert_eq!(Response::Ok("x".to_string()).to_wire(), "OK x\n");
        assert_eq!(Response::Err("y".to_string()).to_wire(), "ERR y\n");
    }

    #[test]
    fn read_request_reads_until_eof_and_trims() {
        let mut cursor = std::io::Cursor::new(b"  left  ".to_vec());
        assert_eq!(read_request(&mut cursor).unwrap(), "left");
    }

    #[test]
    fn read_request_accepts_exactly_max_bytes() {
        let mut cursor = std::io::Cursor::new(vec![b'a'; MAX_REQUEST_BYTES]);
        assert_eq!(
            read_request(&mut cursor).unwrap(),
            "a".repeat(MAX_REQUEST_BYTES)
        );
    }

    #[test]
    fn read_request_rejects_oversized_requests() {
        let mut cursor = std::io::Cursor::new(vec![b'a'; MAX_REQUEST_BYTES + 1]);
        let err = read_request(&mut cursor).unwrap_err();
        assert_eq!(err.to_string(), "request exceeds 128 bytes");
    }

    #[test]
    fn read_request_rejects_invalid_utf8() {
        let mut cursor = std::io::Cursor::new(vec![0xff, 0xfe]);
        let err = read_request(&mut cursor).unwrap_err();
        assert_eq!(err.to_string(), "reading socket request");
    }
}
