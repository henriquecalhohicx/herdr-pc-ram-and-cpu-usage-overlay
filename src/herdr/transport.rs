//! Byte-stream transport to the herdr host. Unix: a Unix-domain socket.
//! Windows: herdr's named pipe (name from `HERDR_SOCKET_PATH`).

use std::io;
use std::path::Path;

#[cfg(unix)]
mod imp {
    use super::*;
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    /// Round-trip timeout guard against a wedged host (Unix only; see the
    /// Windows note in this module).
    const IO_TIMEOUT: Duration = Duration::from_secs(15);

    pub type Transport = UnixStream;

    pub fn connect(path: &Path) -> io::Result<Transport> {
        UnixStream::connect(path)
    }

    pub fn try_clone(t: &Transport) -> io::Result<Transport> {
        t.try_clone()
    }

    pub fn configure(t: &Transport) -> io::Result<()> {
        t.set_read_timeout(Some(IO_TIMEOUT))?;
        t.set_write_timeout(Some(IO_TIMEOUT))?;
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::io::{Read, Write};
    use windows_sys::Win32::Foundation::{
        CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, ERROR_PIPE_BUSY, GENERIC_READ,
        GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    /// Connect timeout while the pipe is momentarily busy (ms).
    const PIPE_WAIT_MS: u32 = 15_000;

    /// A synchronous, byte-mode named-pipe client handle.
    ///
    /// Limitation vs the Unix path: no per-read/write timeout is applied. herdr
    /// answers in milliseconds, so the wedge-guard the Unix socket gets from
    /// `set_read_timeout` is omitted here; a truly hung host would block a
    /// round-trip. Acceptable for v1 (see plan §Task 3).
    pub struct PipeStream(HANDLE);

    // SAFETY: the handle is only used from the thread that owns the struct (and
    // its clone); no concurrent access to a single handle.
    unsafe impl Send for PipeStream {}

    pub type Transport = PipeStream;

    fn to_wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub fn connect(path: &Path) -> io::Result<Transport> {
        let name = to_wide(&path.to_string_lossy());
        loop {
            // SAFETY: name is NUL-terminated; return value checked.
            let h = unsafe {
                CreateFileW(
                    name.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    0,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    0,
                    std::ptr::null_mut(),
                )
            };
            if h != INVALID_HANDLE_VALUE && !h.is_null() {
                return Ok(PipeStream(h));
            }
            let err = io::Error::last_os_error();
            if err.raw_os_error() == Some(ERROR_PIPE_BUSY as i32) {
                // SAFETY: name is NUL-terminated.
                let waited = unsafe { WaitNamedPipeW(name.as_ptr(), PIPE_WAIT_MS) };
                if waited == 0 {
                    return Err(io::Error::last_os_error());
                }
                continue;
            }
            return Err(err);
        }
    }

    pub fn try_clone(t: &Transport) -> io::Result<Transport> {
        let mut dup: HANDLE = std::ptr::null_mut();
        // SAFETY: duplicates our own handle into our own process.
        let ok = unsafe {
            DuplicateHandle(
                GetCurrentProcess(),
                t.0,
                GetCurrentProcess(),
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            )
        };
        if ok == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(PipeStream(dup))
        }
    }

    pub fn configure(_t: &Transport) -> io::Result<()> {
        Ok(()) // see PipeStream limitation note
    }

    impl Read for PipeStream {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let mut read: u32 = 0;
            // SAFETY: buf is valid for buf.len() bytes; read is caller-owned.
            let ok = unsafe {
                ReadFile(
                    self.0,
                    buf.as_mut_ptr(),
                    buf.len() as u32,
                    &mut read,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(read as usize)
            }
        }
    }

    impl Write for PipeStream {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let mut written: u32 = 0;
            // SAFETY: buf is valid for buf.len() bytes; written is caller-owned.
            let ok = unsafe {
                WriteFile(
                    self.0,
                    buf.as_ptr(),
                    buf.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };
            if ok == 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(written as usize)
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Drop for PipeStream {
        fn drop(&mut self) {
            // SAFETY: handle from CreateFileW/DuplicateHandle, closed once.
            unsafe { CloseHandle(self.0) };
        }
    }

    #[cfg(test)]
    pub fn pipe_stream_from_raw_for_test(h: HANDLE) -> PipeStream {
        PipeStream(h)
    }
}

pub use imp::{configure, connect, try_clone, Transport};

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::thread;
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    #[test]
    fn pipe_round_trips_newline_framed_json() {
        let name = r"\\.\pipe\space-usage-test-roundtrip";
        let wname = wide(name);
        // Server: create the pipe, wait for the client, echo one line back.
        let server = thread::spawn(move || {
            let h = unsafe {
                CreateNamedPipeW(
                    wname.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                    1,
                    512,
                    512,
                    0,
                    std::ptr::null(),
                )
            };
            assert!(h as isize != -1);
            unsafe { ConnectNamedPipe(h, std::ptr::null_mut()) };
            let mut srv = imp::pipe_stream_from_raw_for_test(h);
            let mut r = BufReader::new(try_clone(&srv).unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            srv.write_all(line.as_bytes()).unwrap();
        });
        // The server thread may not have created the pipe yet (CreateFileW
        // then returns ERROR_FILE_NOT_FOUND), so retry connect() for up to
        // ~2s instead of a fixed sleep, to avoid a race-driven flake.
        let mut client = {
            let mut attempt = None;
            for _ in 0..40 {
                match connect(std::path::Path::new(name)) {
                    Ok(c) => {
                        attempt = Some(c);
                        break;
                    }
                    Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
                }
            }
            attempt.expect("pipe never became available for connect()")
        };
        client.write_all(b"{\"id\":\"1\"}\n").unwrap();
        let mut r = BufReader::new(try_clone(&client).unwrap());
        let mut got = String::new();
        r.read_line(&mut got).unwrap();
        assert_eq!(got, "{\"id\":\"1\"}\n");
        server.join().unwrap();
    }
}
