//! Windows Live TV uses the app's private LibVLC runtime and adaptive demuxer.
//! VLC receives the original HLS URL: ad segments, discontinuities, alternate
//! audio and initialization changes stay under one playback engine's control.
//! Video frames are presented in the WebView to avoid native HWND airspace.

use crate::vlc_frames::{self, VideoFrames};
use libloading::os::windows::{
    Library, LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR, LOAD_LIBRARY_SEARCH_SYSTEM32,
};
use serde::Serialize;
use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::PathBuf,
    sync::Mutex,
};
use tauri::{AppHandle, Manager};

type Instance = *mut c_void;
type Player = *mut c_void;
type Media = *mut c_void;

macro_rules! api {
    ($($name:ident: $signature:ty),+ $(,)?) => {
        struct Api { $($name: $signature,)+ _library: Library, _core: Library }
        impl Api {
            unsafe fn load(directory: &std::path::Path) -> Result<Self, String> {
                let flags = LOAD_LIBRARY_SEARCH_DLL_LOAD_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32;
                let core = Library::load_with_flags(directory.join("libvlccore.dll"), flags).map_err(|e| e.to_string())?;
                let library = Library::load_with_flags(directory.join("libvlc.dll"), flags).map_err(|e| e.to_string())?;
                $(let $name = *library.get::<$signature>(concat!(stringify!($name), "\0").as_bytes()).map_err(|e| e.to_string())?;)+
                Ok(Self { $($name,)+ _library: library, _core: core })
            }
        }
    }
}

api! {
    libvlc_get_version: unsafe extern "C" fn() -> *const c_char,
    libvlc_errmsg: unsafe extern "C" fn() -> *const c_char,
    libvlc_new: unsafe extern "C" fn(c_int, *const *const c_char) -> Instance,
    libvlc_release: unsafe extern "C" fn(Instance),
    libvlc_media_new_location: unsafe extern "C" fn(Instance, *const c_char) -> Media,
    libvlc_media_add_option: unsafe extern "C" fn(Media, *const c_char),
    libvlc_media_release: unsafe extern "C" fn(Media),
    libvlc_media_player_new: unsafe extern "C" fn(Instance) -> Player,
    libvlc_media_player_release: unsafe extern "C" fn(Player),
    libvlc_media_player_set_media: unsafe extern "C" fn(Player, Media),
    libvlc_video_set_callbacks: unsafe extern "C" fn(Player,
        Option<unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> *mut c_void>,
        Option<unsafe extern "C" fn(*mut c_void, *mut c_void, *const *mut c_void)>,
        Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>, *mut c_void),
    libvlc_video_set_format_callbacks: unsafe extern "C" fn(Player,
        Option<unsafe extern "C" fn(*mut *mut c_void, *mut c_char, *mut u32, *mut u32, *mut u32, *mut u32) -> u32>,
        Option<unsafe extern "C" fn(*mut c_void)>),
    libvlc_media_player_play: unsafe extern "C" fn(Player) -> c_int,
    libvlc_media_player_stop: unsafe extern "C" fn(Player),
    libvlc_media_player_set_pause: unsafe extern "C" fn(Player, c_int),
    libvlc_media_player_get_state: unsafe extern "C" fn(Player) -> c_int,
    libvlc_media_player_get_time: unsafe extern "C" fn(Player) -> i64,
    libvlc_audio_set_volume: unsafe extern "C" fn(Player, c_int) -> c_int,
    libvlc_audio_set_mute: unsafe extern "C" fn(Player, c_int),
    libvlc_video_set_mouse_input: unsafe extern "C" fn(Player, u32),
    libvlc_video_set_key_input: unsafe extern "C" fn(Player, u32),
}

struct Engine {
    api: Api,
    instance: Instance,
    player: Player,
    session: String,
    wake: Option<keepawake::KeepAwake>,
    frames: Box<VideoFrames>,
}

// LibVLC supports calls from application threads. All calls and lifetime
// changes here are serialized by VlcState::engine; VLC owns its decoder threads.
unsafe impl Send for Engine {}

impl Engine {
    unsafe fn new(directory: &std::path::Path) -> Result<Self, String> {
        let api = Api::load(directory)
            .map_err(|e| format!("Could not load the bundled VLC runtime: {e}"))?;
        // LibVLC locates modules independently of DLL search. Never use a user's
        // installed VLC or PATH; all modules come from this packaged directory.
        std::env::set_var("VLC_PLUGIN_PATH", directory.join("plugins"));
        let args: Vec<CString> = [
            "--ignore-config",
            "--no-media-library",
            "--no-osd",
            "--no-video-title-show",
            "--no-plugins-cache",
            "--network-caching=3000",
            "--no-snapshot-preview",
        ]
        .iter()
        .map(|s| CString::new(*s).unwrap())
        .collect();
        let pointers: Vec<_> = args.iter().map(|s| s.as_ptr()).collect();
        let instance = (api.libvlc_new)(pointers.len() as c_int, pointers.as_ptr());
        if instance.is_null() {
            return Err("The bundled VLC runtime could not initialize.".into());
        }
        let player = (api.libvlc_media_player_new)(instance);
        if player.is_null() {
            (api.libvlc_release)(instance);
            return Err("Could not create the Live TV player.".into());
        }
        (api.libvlc_video_set_mouse_input)(player, 0);
        (api.libvlc_video_set_key_input)(player, 0);
        let frames = Box::new(VideoFrames::default());
        (api.libvlc_video_set_callbacks)(
            player,
            Some(vlc_frames::lock),
            None,
            Some(vlc_frames::display),
            (&*frames as *const VideoFrames).cast_mut().cast(),
        );
        (api.libvlc_video_set_format_callbacks)(
            player,
            Some(vlc_frames::setup),
            Some(vlc_frames::cleanup),
        );
        log::info!(
            "[live-vlc] bundled runtime {}",
            CStr::from_ptr((api.libvlc_get_version)()).to_string_lossy()
        );
        Ok(Self {
            api,
            instance,
            player,
            session: String::new(),
            wake: None,
            frames,
        })
    }

    unsafe fn play(
        &mut self,
        url: &str,
        session: String,
        volume: u32,
        muted: bool,
    ) -> Result<(), String> {
        let url = CString::new(url).map_err(|_| "Invalid stream URL")?;
        (self.api.libvlc_media_player_stop)(self.player);
        self.frames.clear();
        let media = (self.api.libvlc_media_new_location)(self.instance, url.as_ptr());
        if media.is_null() {
            return Err("Could not open this channel.".into());
        }
        for option in [":network-caching=3000", ":http-reconnect"] {
            (self.api.libvlc_media_add_option)(media, CString::new(option).unwrap().as_ptr());
        }
        (self.api.libvlc_media_player_set_media)(self.player, media);
        (self.api.libvlc_media_release)(media);
        self.session = session;
        if (self.api.libvlc_media_player_play)(self.player) != 0 {
            let error = (self.api.libvlc_errmsg)();
            return Err(if error.is_null() {
                "Could not start this channel.".into()
            } else {
                CStr::from_ptr(error).to_string_lossy().into_owned()
            });
        }
        (self.api.libvlc_audio_set_volume)(self.player, volume.min(100) as c_int);
        (self.api.libvlc_audio_set_mute)(self.player, muted as c_int);
        self.wake = keepawake::Builder::default()
            .display(true)
            .idle(false)
            .create()
            .ok();
        Ok(())
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            (self.api.libvlc_media_player_stop)(self.player);
            (self.api.libvlc_media_player_release)(self.player);
            (self.api.libvlc_release)(self.instance);
        }
    }
}

#[derive(Default)]
struct Request {
    generation: u64,
    session: String,
}

#[derive(Default)]
pub struct VlcState {
    engine: Mutex<Option<Engine>>,
    request: Mutex<Request>,
}

fn runtime_directory(app: &AppHandle) -> Result<PathBuf, String> {
    let directory = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("vlc");
    if !directory.join("libvlc.dll").is_file()
        || !directory
            .join("plugins/demux/libadaptive_plugin.dll")
            .is_file()
    {
        return Err(
            "This Nola build is missing its bundled Live TV runtime. Reinstall Nola to restore it."
                .into(),
        );
    }
    Ok(directory)
}

#[tauri::command]
pub async fn vlc_load(
    app: AppHandle,
    session: String,
    url: String,
    volume: u32,
    muted: bool,
) -> Result<(), String> {
    let parsed = url::Url::parse(&url).map_err(|_| "Invalid channel URL")?;
    if !matches!(
        parsed.scheme(),
        "http" | "https" | "rtsp" | "rtmp" | "udp" | "rtp"
    ) {
        return Err("Unsupported channel URL".into());
    }
    let ticket = {
        let state = app.state::<VlcState>();
        let mut request = state.request.lock().map_err(|e| e.to_string())?;
        request.generation += 1;
        request.session = session.clone();
        request.generation
    };
    let directory = runtime_directory(&app)?;
    let handle = app.clone();
    let id = session.clone();
    tokio::task::spawn_blocking(move || {
        let state = handle.state::<VlcState>();
        let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
        if state.request.lock().map_err(|e| e.to_string())?.generation != ticket {
            return Err("Channel request was cancelled.".to_string());
        }
        if engine.is_none() {
            *engine = Some(unsafe { Engine::new(&directory)? });
        }
        unsafe { engine.as_mut().unwrap().play(&url, id, volume, muted) }
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(())
}

#[derive(Serialize)]
pub struct PlaybackStatus {
    state: &'static str,
    time: f64,
    video_frames: u32,
}

#[tauri::command]
pub async fn vlc_status(app: AppHandle, session: String) -> Result<Option<PlaybackStatus>, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<VlcState>();
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        let Some(engine) = engine.as_ref().filter(|e| e.session == session) else {
            return Ok(None);
        };
        unsafe {
            let status = match (engine.api.libvlc_media_player_get_state)(engine.player) {
                1 => "opening",
                2 => "buffering",
                3 => "playing",
                4 => "paused",
                5 => "stopped",
                6 => "ended",
                7 => "error",
                _ => "idle",
            };
            Ok(Some(PlaybackStatus {
                state: status,
                video_frames: engine.frames.count(),
                time: (engine.api.libvlc_media_player_get_time)(engine.player).max(0) as f64
                    / 1000.0,
            }))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn vlc_frame(
    app: AppHandle,
    session: String,
    after: u32,
) -> Result<tauri::ipc::Response, String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<VlcState>();
        let engine = state.engine.lock().map_err(|e| e.to_string())?;
        let bytes = engine
            .as_ref()
            .filter(|engine| engine.session == session)
            .map(|engine| engine.frames.snapshot(after))
            .unwrap_or_default();
        Ok(tauri::ipc::Response::new(bytes))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn vlc_control(
    app: AppHandle,
    session: String,
    paused: Option<bool>,
    volume: Option<u32>,
    muted: Option<bool>,
) -> Result<(), String> {
    tokio::task::spawn_blocking(move || {
        let state = app.state::<VlcState>();
        let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
        let Some(engine) = engine.as_mut().filter(|e| e.session == session) else {
            return Ok(());
        };
        unsafe {
            if let Some(paused) = paused {
                (engine.api.libvlc_media_player_set_pause)(engine.player, paused as c_int);
                engine.wake = if paused {
                    None
                } else {
                    keepawake::Builder::default()
                        .display(true)
                        .idle(false)
                        .create()
                        .ok()
                };
            }
            if let Some(volume) = volume {
                (engine.api.libvlc_audio_set_volume)(engine.player, volume.min(100) as c_int);
            }
            if let Some(muted) = muted {
                (engine.api.libvlc_audio_set_mute)(engine.player, muted as c_int);
            }
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn vlc_stop(app: AppHandle, session: String) -> Result<(), String> {
    {
        let state = app.state::<VlcState>();
        let mut request = state.request.lock().map_err(|e| e.to_string())?;
        if request.session == session {
            request.generation += 1;
        }
    }
    let handle = app.clone();
    let id = session.clone();
    tokio::task::spawn_blocking(move || {
        let state = handle.state::<VlcState>();
        let mut engine = state.engine.lock().map_err(|e| e.to_string())?;
        if let Some(engine) = engine.as_mut().filter(|e| e.session == id) {
            unsafe {
                (engine.api.libvlc_media_player_stop)(engine.player);
                engine.wake = None;
            }
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_runtime_loads_and_initializes_without_system_vlc() {
        // No media or network playback: verifies the packaged ABI and options.
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("libs/vlc");
        let engine = unsafe { Engine::new(&directory) }.expect("bundled VLC must initialize");
        let version =
            unsafe { CStr::from_ptr((engine.api.libvlc_get_version)()) }.to_string_lossy();
        assert!(
            version.starts_with("3.0.24"),
            "unexpected runtime: {version}"
        );
        assert!(!engine.player.is_null());
    }

    #[test]
    fn bundled_vlc_decodes_pixels_and_reconfigures_video_dimensions() {
        use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
        let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("libs/vlc");
        let mut engine = unsafe { Engine::new(&directory) }.unwrap();
        for (width, height) in [(7u32, 5u32), (12, 8)] {
            // Small local BMP fixture: no live network or commercial-break test.
            let stride = (width * 3 + 3) & !3;
            let image_size = stride * height;
            let mut bmp = vec![0u8; 54 + image_size as usize];
            bmp[..2].copy_from_slice(b"BM");
            bmp[2..6].copy_from_slice(&(54 + image_size).to_le_bytes());
            bmp[10..14].copy_from_slice(&54u32.to_le_bytes());
            bmp[14..18].copy_from_slice(&40u32.to_le_bytes());
            bmp[18..22].copy_from_slice(&width.to_le_bytes());
            bmp[22..26].copy_from_slice(&height.to_le_bytes());
            bmp[26..28].copy_from_slice(&1u16.to_le_bytes());
            bmp[28..30].copy_from_slice(&24u16.to_le_bytes());
            for row in 0..height as usize {
                for col in 0..width as usize {
                    let start = 54 + row * stride as usize + col * 3;
                    bmp[start..start + 3].copy_from_slice(&[11, 31, 241]);
                }
            }
            let path = std::env::temp_dir().join(format!(
                "nola-video-{}.bmp",
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::write(&path, bmp).unwrap();
            let url = url::Url::from_file_path(&path).unwrap();
            unsafe { engine.play(url.as_str(), "render-check".into(), 0, true) }.unwrap();
            let deadline = Instant::now() + Duration::from_secs(8);
            while engine.frames.count() == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let frame = engine.frames.snapshot(0);
            unsafe {
                (engine.api.libvlc_media_player_stop)(engine.player);
            }
            std::fs::remove_file(path).unwrap();
            assert!(
                frame.len() >= 20,
                "VLC must deliver decoded video pixels, not merely initialize"
            );
            assert_eq!(u32::from_le_bytes(frame[4..8].try_into().unwrap()), width);
            assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()), height);
            assert_eq!(
                &frame[16..20],
                &[241, 31, 11, 255],
                "video must reach the canvas as opaque RGBA"
            );
        }
    }

    #[test]
    fn installer_resources_preserve_vlc_plugin_directories() {
        use std::collections::{HashMap, HashSet};
        use tauri::utils::resources::ResourcePaths;
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.vlc.windows.json")).unwrap();
        let mappings: HashMap<String, String> = config["bundle"]["resources"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(source, target)| {
                (
                    root.join(source).to_string_lossy().into_owned(),
                    target.as_str().unwrap().to_string(),
                )
            })
            .collect();
        let resources: Vec<_> = ResourcePaths::from_map(&mappings, true)
            .iter()
            .map(Result::unwrap)
            .collect();
        let targets: HashSet<PathBuf> =
            resources.iter().map(|r| r.target().to_path_buf()).collect();
        for file in [
            "vlc/libvlc.dll",
            "vlc/libvlccore.dll",
            "vlc/plugins/demux/libadaptive_plugin.dll",
            "vlc/plugins/video_output/libdirect3d11_plugin.dll",
            "vlc/plugins/audio_output/libmmdevice_plugin.dll",
            "vlc/COPYING.txt",
            "vlc/runtime-source.json",
        ] {
            assert!(
                targets.contains(&PathBuf::from(file)),
                "missing installer resource {file}"
            );
        }
        let manifest: serde_json::Value = serde_json::from_slice(
            &std::fs::read(root.join("libs/vlc/runtime-source.json")).unwrap(),
        )
        .unwrap();
        for file in manifest["files"].as_object().unwrap().keys() {
            assert!(
                targets.contains(&PathBuf::from("vlc").join(file)),
                "runtime DLL omitted from installer: {file}"
            );
        }
    }
}
