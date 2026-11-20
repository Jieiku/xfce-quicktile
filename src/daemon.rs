//! Unix-socket daemon and one-shot CLI client.

use std::{
    env, fs,
    io::{Read, Write},
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    str::FromStr,
};

use anyhow::{bail, Context, Result};

use crate::{protocol, tiling::Direction, x11::X11Manager};

/// Socket path: `$XDG_RUNTIME_DIR` when set, otherwise a per-user `/tmp`
/// directory; the filename encodes the (sanitized) display so multiple X
/// servers on one host do not collide.
fn socket_path() -> PathBuf {
    let runtime = env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/xfce-quicktile-{}", libc_uid())));
    let display = env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
    let safe_display: String = display
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    runtime.join(format!("xfce-quicktile-{safe_display}.sock"))
}

// getuid(2), declared directly to avoid another crate dependency (the
// value is only used to scope the /tmp socket-path fallback). Safe to
// call: it is a pure query that returns the caller's uid and takes no
// pointer arguments, so there are no aliasing or UB concerns.
extern "C" {
    fn getuid() -> u32;
}
fn libc_uid() -> u32 {
    unsafe { getuid() }
}

/// Run the daemon until the process is killed.
pub fn run_daemon() -> Result<()> {
    let path = socket_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating socket directory {}", parent.display()))?;
    }

    if path.exists() {
        match UnixStream::connect(&path) {
            Ok(_) => bail!(
                "xfce-quicktile daemon already appears to be running ({})",
                path.display()
            ),
            Err(_) => fs::remove_file(&path)
                .with_context(|| format!("removing stale socket {}", path.display()))?,
        }
    }

    let listener =
        UnixListener::bind(&path).with_context(|| format!("binding socket {}", path.display()))?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    eprintln!("xfce-quicktile daemon listening on {}", path.display());

    let mut manager = X11Manager::connect()?;
    for incoming in listener.incoming() {
        match incoming {
            Ok(mut stream) => {
                let response = match handle_command(&mut stream, &mut manager) {
                    Ok(message) => protocol::Response::Ok(message),
                    Err(error) => protocol::Response::Err(format!("{error:#}")),
                };
                // A write error just means the client went away; the
                // connection is dropped (closed) right after.
                let _ = stream.write_all(response.to_wire().as_bytes());
            }
            Err(error) => eprintln!("socket accept error: {error}"),
        }
    }

    Ok(())
}

/// Parse one request and dispatch it. The returned string is the `OK`
/// payload; any `Err` is rendered as an `ERR` reply by the caller.
fn handle_command(stream: &mut UnixStream, manager: &mut X11Manager) -> Result<String> {
    let request = protocol::read_request(stream)?;
    let command = protocol::Command::from_str(&request).map_err(anyhow::Error::msg)?;
    match command {
        protocol::Command::Ping => Ok("daemon is running".to_string()),
        protocol::Command::Stop => Ok("send SIGTERM to the daemon to stop it".to_string()),
        protocol::Command::Left => manager.handle(Direction::Left),
        protocol::Command::Right => manager.handle(Direction::Right),
        protocol::Command::Up => manager.handle(Direction::Up),
        protocol::Command::Down => manager.handle(Direction::Down),
    }
}

/// Send one command to the daemon and print its reply.
///
/// The request is the command's wire name with no trailing newline; the
/// write-side shutdown delimits it. The response read is unbounded, which
/// is fine here — the daemon is our own process replying with one short
/// line.
pub fn send_command(command: protocol::Command) -> Result<()> {
    let path = socket_path();
    let mut stream = UnixStream::connect(&path).with_context(|| {
        format!(
            "cannot connect to daemon at {}; start it with `xfce-quicktile daemon`",
            path.display()
        )
    })?;
    stream.write_all(command.as_str().as_bytes())?;
    stream.shutdown(std::net::Shutdown::Write)?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    match protocol::Response::from_wire(response.trim())? {
        protocol::Response::Ok(message) => {
            println!("{message}");
            Ok(())
        }
        protocol::Response::Err(message) => bail!("{message}"),
    }
}
