//! Best-effort alert sound. Windows: winmm `PlaySoundW` (async, fire-and-forget).
//! Every other OS: a no-op, so the Linux CI leg stays clean.

use std::path::Path;

/// Play a `.wav` asynchronously, best-effort. Empty path or a missing file is a
/// no-op; a playback failure is ignored. On non-Windows this does nothing.
pub fn play_wav(path: &str) {
    if path.is_empty() || !Path::new(path).exists() {
        return;
    }
    play(path);
}

#[cfg(windows)]
fn play(path: &str) {
    use windows_sys::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_FILENAME};
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `wide` is NUL-terminated and outlives the call; SND_ASYNC returns
    // immediately (winmm copies the name); null module handle = load from file.
    unsafe {
        PlaySoundW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            SND_FILENAME | SND_ASYNC,
        );
    }
}

#[cfg(not(windows))]
fn play(_path: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_path_is_noop() {
        play_wav(""); // must not panic
    }

    #[test]
    fn missing_file_is_noop() {
        play_wav("this/path/does/not/exist/nope.wav"); // must not panic
    }
}
