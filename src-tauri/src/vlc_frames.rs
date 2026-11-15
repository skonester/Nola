//! VLC's vmem output, independent of HWND child-window composition. The latest
//! presented RGBA frame is transferred as binary IPC data to the WebView canvas.
//! No frame queue grows while the UI is busy; older pictures are replaced.

use std::{
    alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout},
    collections::HashMap,
    ffi::{c_char, c_void},
    ptr::{self, NonNull},
    sync::{Arc, Mutex},
};

const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

struct Pixels {
    pointer: NonNull<u8>,
    layout: Layout,
}

// A picture is writable only between VLC's lock/display callbacks. After
// display transfers ownership into Arc<Picture>, all readers are immutable.
unsafe impl Send for Pixels {}
unsafe impl Sync for Pixels {}

impl Pixels {
    fn new(length: usize) -> Self {
        let layout = Layout::from_size_align(length, 32).unwrap();
        let pointer = NonNull::new(unsafe { alloc_zeroed(layout) })
            .unwrap_or_else(|| handle_alloc_error(layout));
        Self { pointer, layout }
    }
    fn bytes(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.layout.size()) }
    }
}

impl Drop for Pixels {
    fn drop(&mut self) {
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) }
    }
}

#[derive(Clone, Copy)]
struct Format {
    width: u32,
    height: u32,
    pitch: usize,
    length: usize,
}

struct Picture {
    pixels: Pixels,
    format: Format,
    serial: u32,
}

#[derive(Default)]
struct FrameState {
    format: Option<Format>,
    latest: Option<Arc<Picture>>,
    spare: Vec<Pixels>,
    serial: u32,
    pending: HashMap<usize, Box<Picture>>,
}

#[derive(Default)]
pub struct VideoFrames {
    state: Mutex<FrameState>,
}

impl VideoFrames {
    pub fn clear(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.latest = None;
        }
    }

    pub fn count(&self) -> u32 {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.latest.as_ref().map(|p| p.serial))
            .unwrap_or(0)
    }

    /// NLV1, width, height, serial (little endian), tightly packed opaque RGBA.
    pub fn snapshot(&self, after: u32) -> Vec<u8> {
        let picture = self.state.lock().ok().and_then(|s| s.latest.clone());
        let Some(picture) = picture.filter(|p| p.serial != after) else {
            return Vec::new();
        };
        let Format {
            width,
            height,
            pitch,
            ..
        } = picture.format;
        let row_bytes = width as usize * 4;
        let mut data = Vec::with_capacity(16 + row_bytes * height as usize);
        data.extend_from_slice(b"NLV1");
        for value in [width, height, picture.serial] {
            data.extend_from_slice(&value.to_le_bytes());
        }
        let bytes = picture.pixels.bytes();
        for row in 0..height as usize {
            data.extend_from_slice(&bytes[row * pitch..row * pitch + row_bytes]);
        }
        // TV pictures are opaque even if a source/converter leaves alpha unset.
        for pixel in data[16..].chunks_exact_mut(4) {
            pixel[3] = 255;
        }
        data
    }
}

pub unsafe extern "C" fn setup(
    opaque: *mut *mut c_void,
    chroma: *mut c_char,
    width: *mut u32,
    height: *mut u32,
    pitches: *mut u32,
    lines: *mut u32,
) -> u32 {
    if opaque.is_null() || (*opaque).is_null() || width.is_null() || height.is_null() {
        return 0;
    }
    let (w, h) = (*width, *height);
    if w == 0 || h == 0 {
        return 0;
    }
    let Some(pitch) = (w as usize)
        .checked_mul(4)
        .and_then(|n| n.checked_add(31))
        .map(|n| n & !31)
    else {
        return 0;
    };
    let Some(aligned_height) = (h as usize).checked_add(31).map(|n| n & !31) else {
        return 0;
    };
    let Some(length) = pitch
        .checked_mul(aligned_height)
        .filter(|n| *n <= MAX_FRAME_BYTES)
    else {
        return 0;
    };
    let frames = &*((*opaque).cast::<VideoFrames>());
    let Ok(mut state) = frames.state.lock() else {
        return 0;
    };
    state.format = Some(Format {
        width: w,
        height: h,
        pitch,
        length,
    });
    state.spare.retain(|p| p.layout.size() == length);
    ptr::copy_nonoverlapping(b"RGBA".as_ptr(), chroma.cast(), 4);
    *pitches = pitch as u32;
    *lines = aligned_height as u32;
    1
}

pub unsafe extern "C" fn lock(opaque: *mut c_void, planes: *mut *mut c_void) -> *mut c_void {
    let frames = &*opaque.cast::<VideoFrames>();
    let mut state = frames.state.lock().unwrap_or_else(|e| e.into_inner());
    let format = state
        .format
        .expect("VLC calls format setup before locking a picture");
    let pixels = state
        .spare
        .pop()
        .filter(|p| p.layout.size() == format.length)
        .unwrap_or_else(|| Pixels::new(format.length));
    *planes = pixels.pointer.as_ptr().cast();
    let picture = Box::new(Picture {
        pixels,
        format,
        serial: 0,
    });
    let token = (&*picture as *const Picture) as usize;
    state.pending.insert(token, picture);
    token as *mut c_void
}

pub unsafe extern "C" fn display(opaque: *mut c_void, picture: *mut c_void) {
    if picture.is_null() {
        return;
    }
    let frames = &*opaque.cast::<VideoFrames>();
    let mut state = frames.state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(picture) = state.pending.remove(&(picture as usize)) else {
        return;
    };
    let mut picture = *picture;
    state.serial = state.serial.wrapping_add(1).max(1);
    picture.serial = state.serial;
    if let Some(old) = state.latest.replace(Arc::new(picture)) {
        if let Ok(old) = Arc::try_unwrap(old) {
            if state.spare.len() < 2 {
                state.spare.push(old.pixels);
            }
        }
    }
}

pub unsafe extern "C" fn cleanup(opaque: *mut c_void) {
    // VLC can discard a prepared picture when an output closes at a format
    // transition. Free those buffers after VLC has released the old output.
    let frames = &*opaque.cast::<VideoFrames>();
    if let Ok(mut state) = frames.state.lock() {
        state.pending.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_frame_removes_pitch_padding_and_forces_opaque_alpha() {
        let frames = VideoFrames::default();
        let mut opaque = (&frames as *const VideoFrames).cast_mut().cast();
        let (mut width, mut height, mut pitch, mut lines) = (3, 2, 0, 0);
        let mut chroma = [0i8; 4];
        unsafe {
            assert_eq!(
                setup(
                    &mut opaque,
                    chroma.as_mut_ptr(),
                    &mut width,
                    &mut height,
                    &mut pitch,
                    &mut lines
                ),
                1
            );
            assert_eq!(pitch % 32, 0);
            let mut plane = ptr::null_mut();
            let picture = lock(opaque, &mut plane);
            assert_eq!(plane as usize % 32, 0);
            for row in 0..height as usize {
                for col in 0..width as usize {
                    ptr::copy_nonoverlapping(
                        [250, 20, 10, 0].as_ptr(),
                        plane.cast::<u8>().add(row * pitch as usize + col * 4),
                        4,
                    );
                }
            }
            display(opaque, picture);
        }
        let frame = frames.snapshot(0);
        assert_eq!(&frame[..4], b"NLV1");
        assert_eq!(frame.len(), 16 + 3 * 2 * 4);
        assert!(frame[16..]
            .chunks_exact(4)
            .all(|pixel| pixel == [250, 20, 10, 255]));
        assert!(frames.snapshot(frames.count()).is_empty());
    }
}
