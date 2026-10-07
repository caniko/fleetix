//! One deadline covers connect, complete request delivery and complete reply.
use super::{MAX_FRAME, same_uid};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::{ffi::OsStrExt, net::UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

// Bound both descriptor/memory use and work per event-loop turn. Slow delivery
// and slow readers never occupy the coordinator's scheduling thread.
pub(super) const MAX_CONNECTIONS: usize = 64;
const IO_TIMEOUT: Duration = Duration::from_secs(5);
const IO_CHUNK: usize = 8192;

pub(super) struct Incoming {
    pub stream: UnixStream,
    raw: Vec<u8>,
    deadline: Instant,
}

impl Incoming {
    pub fn new(stream: UnixStream) -> Result<Self, String> {
        stream.set_nonblocking(true).map_err(|e| e.to_string())?;
        same_uid(&stream)?;
        Ok(Self {
            stream,
            raw: Vec::new(),
            deadline: Instant::now() + IO_TIMEOUT,
        })
    }

    pub fn receive(&mut self) -> Result<Option<Vec<u8>>, String> {
        if Instant::now() >= self.deadline {
            return Err("request delivery deadline exceeded".into());
        }
        let mut buffer = [0u8; IO_CHUNK];
        match self.stream.read(&mut buffer) {
            Ok(0) => Err("truncated protocol frame".into()),
            Ok(size) => {
                let frame = &buffer[..size];
                let newline = frame.iter().position(|byte| *byte == b'\n');
                self.raw
                    .extend_from_slice(&frame[..newline.map_or(size, |n| n + 1)]);
                if self.raw.len() as u64 > MAX_FRAME {
                    return Err("oversized protocol frame".into());
                }
                if newline.is_some() {
                    Ok(Some(std::mem::take(&mut self.raw)))
                } else {
                    Ok(None)
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

pub(super) struct Outgoing {
    stream: UnixStream,
    raw: Vec<u8>,
    written: usize,
    deadline: Instant,
}

impl Outgoing {
    pub fn new(stream: UnixStream, raw: Vec<u8>) -> Self {
        Self {
            stream,
            raw,
            written: 0,
            deadline: Instant::now() + IO_TIMEOUT,
        }
    }

    /// A disconnect/timeout detaches the client, never undoing durable state.
    pub fn pending(&mut self) -> bool {
        if Instant::now() >= self.deadline {
            return false;
        }
        let end = (self.written + IO_CHUNK).min(self.raw.len());
        match self.stream.write(&self.raw[self.written..end]) {
            Ok(0) => false,
            Ok(size) => {
                self.written += size;
                self.written < self.raw.len()
            }
            Err(e) => matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ),
        }
    }
}

/// Observe full disconnect/error without reading, writing or treating a peer's
/// write-half shutdown as detachment while it still waits to read our reply.
pub(super) fn connected(stream: &UnixStream) -> bool {
    let mut descriptor = libc::pollfd {
        fd: stream.as_raw_fd(),
        events: 0,
        revents: 0,
    };
    // SAFETY: the borrowed stream keeps its descriptor live; poll receives one
    // initialized writable entry and a zero timeout, so this never blocks.
    let result = unsafe { libc::poll(&mut descriptor, 1, 0) };
    if result < 0 {
        return std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted;
    }
    descriptor.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) == 0
}

fn wait(
    stream: &UnixStream,
    events: i16,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Result<(), String> {
    loop {
        if stop.is_some_and(|stop| stop.load(Ordering::Relaxed)) {
            return Err("coordinator IPC interrupted".into());
        }
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or("coordinator IPC deadline exceeded")?;
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        let remaining = if stop.is_some() {
            remaining.min(Duration::from_millis(100))
        } else {
            remaining
        };
        let milliseconds = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        // SAFETY: the live stream owns the descriptor and poll receives one
        // initialized, writable pollfd. No descriptor crosses the call boundary.
        let result = unsafe { libc::poll(&mut descriptor, 1, milliseconds) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error.to_string());
        }
        if result > 0 {
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err("invalid coordinator socket descriptor".into());
            }
            return Ok(());
        }
    }
}

fn connect(
    path: &Path,
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Result<UnixStream, String> {
    let name = path.as_os_str().as_bytes();
    // SAFETY: sockaddr_un consists entirely of integer fields and a byte array.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if name.is_empty() || name.len() >= address.sun_path.len() || name.contains(&0) {
        return Err("invalid coordinator socket path".into());
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, byte) in address.sun_path.iter_mut().zip(name) {
        *destination = *byte as libc::c_char;
    }
    // SAFETY: socket returns a fresh descriptor, adopted exactly once below.
    let raw = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if raw < 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    // SAFETY: raw is a newly created, valid and exclusively owned descriptor.
    let stream = UnixStream::from(unsafe { OwnedFd::from_raw_fd(raw) });
    // SAFETY: address is initialized, NUL-terminated and lives for this syscall.
    let result = unsafe {
        libc::connect(
            stream.as_raw_fd(),
            (&address as *const libc::sockaddr_un).cast(),
            std::mem::size_of_val(&address) as libc::socklen_t,
        )
    };
    if result < 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINPROGRESS) {
            // Linux Unix-socket backlog saturation returns EAGAIN. Fail closed
            // immediately instead of retrying a potentially unbounded connect.
            return Err(error.to_string());
        }
        wait(&stream, libc::POLLOUT, deadline, stop)?;
        let mut error = 0i32;
        let mut length = std::mem::size_of_val(&error) as libc::socklen_t;
        // SAFETY: initialized, correctly sized writable SO_ERROR storage.
        let result = unsafe {
            libc::getsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                (&mut error as *mut i32).cast(),
                &mut length,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        if error != 0 {
            return Err(std::io::Error::from_raw_os_error(error).to_string());
        }
    }
    same_uid(&stream)?;
    Ok(stream)
}

pub(super) fn exchange(
    socket: &Path,
    mut request: &[u8],
    deadline: Instant,
    stop: Option<&AtomicBool>,
) -> Result<Vec<u8>, String> {
    let mut stream = connect(socket, deadline, stop)?;
    while !request.is_empty() {
        wait(&stream, libc::POLLOUT, deadline, stop)?;
        match stream.write(request) {
            Ok(0) => return Err("coordinator closed during request delivery".into()),
            Ok(size) => request = &request[size..],
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let mut reply = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        wait(&stream, libc::POLLIN, deadline, stop)?;
        match stream.read(&mut buffer) {
            Ok(0) => return Err("truncated coordinator reply".into()),
            Ok(size) => {
                let frame = &buffer[..size];
                let newline = frame.iter().position(|byte| *byte == b'\n');
                reply.extend_from_slice(&frame[..newline.map_or(size, |offset| offset + 1)]);
                if reply.len() as u64 > MAX_FRAME {
                    return Err("oversized coordinator reply".into());
                }
                if newline.is_some() {
                    return Ok(reply);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiter_disconnect_probe_preserves_live_and_write_half_closed_clients() {
        let (server, client) = UnixStream::pair().unwrap();
        assert!(connected(&server));
        client.shutdown(std::net::Shutdown::Write).unwrap();
        assert!(connected(&server));
        drop(client);
        assert!(!connected(&server));
    }
}
