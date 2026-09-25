//! Live TV: M3U channel playlists (Pluto TV, Plex, Samsung TV Plus, Roku, or
//! any user-supplied playlist). Sources and their parsed channels are stored
//! together in `live_channels.json` so the channel list is browsable offline.

use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::OnceLock;
use tauri::{AppHandle, Manager};
use tokio::sync::Mutex;
use url::Url;

const MAX_PLAYLIST_BYTES: usize = 32 * 1024 * 1024;
const STREAM_SCHEMES: &[&str] = &["http", "https", "rtsp", "rtmp", "udp", "rtp"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSource {
    pub id: String,
    pub name: String,
    pub location: String,
    #[serde(default)]
    pub updated_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveChannel {
    pub source_id: String,
    pub name: String,
    pub url: String,
    #[serde(default = "uncategorized")]
    pub group: String,
    #[serde(default)]
    pub logo: String,
    #[serde(default)]
    pub tvg_id: String,
    #[serde(default)]
    pub number: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ChannelData {
    #[serde(default)]
    pub sources: Vec<ChannelSource>,
    #[serde(default)]
    pub channels: Vec<LiveChannel>,
}

fn uncategorized() -> String {
    "Uncategorized".to_string()
}

/// Serializes read-modify-write cycles on the data file.
fn store_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn data_path(app_handle: &AppHandle) -> Result<PathBuf, String> {
    let dir = app_handle
        .path()
        .app_data_dir()
        .map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("live_channels.json"))
}

fn load(path: &PathBuf) -> Result<ChannelData, String> {
    if !path.exists() {
        return Ok(ChannelData::default());
    }
    let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&content).map_err(|_| "Could not read saved live channels.".to_string())
}

fn save(path: &PathBuf, data: &ChannelData) -> Result<(), String> {
    let content = serde_json::to_string(data).map_err(|e| e.to_string())?;
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, content).map_err(|e| e.to_string())?;
    std::fs::rename(&temporary, path).map_err(|_| "Could not save live channels.".to_string())
}

fn new_id() -> String {
    let nanos = chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default();
    format!("{:x}", nanos)
}

fn remote_url(location: &str) -> Option<Url> {
    Url::parse(location)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some())
}

fn local_path(location: &str) -> PathBuf {
    match Url::parse(location) {
        Ok(u) if u.scheme() == "file" => u.to_file_path().unwrap_or_else(|_| PathBuf::from(location)),
        _ => PathBuf::from(location),
    }
}

async fn read_playlist(location: &str) -> Result<String, String> {
    let bytes = if let Some(url) = remote_url(location) {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .user_agent("Nola")
            .build()
            .map_err(|e| e.to_string())?;
        let mut response = client
            .get(url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| e.to_string())?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_PLAYLIST_BYTES {
                return Err("Playlist exceeds 32 MB".into());
            }
        }
        body
    } else {
        let path = local_path(location);
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|_| "Playlist file not found".to_string())?;
        if meta.len() as usize > MAX_PLAYLIST_BYTES {
            return Err("Playlist exceeds 32 MB".into());
        }
        tokio::fs::read(&path).await.map_err(|e| e.to_string())?
    };
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Index of the comma separating EXTINF attributes from the title, ignoring
/// commas inside quoted attribute values.
fn metadata_comma(line: &str) -> Option<usize> {
    let mut quote: Option<char> = None;
    for (index, ch) in line.char_indices() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => {}
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ',' => return Some(index),
            None => {}
        }
    }
    None
}

fn attribute_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"([\w-]+)\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s]+))"#).unwrap()
    })
}

fn has_tag(line: &str, tag: &str) -> bool {
    line.get(..tag.len()).is_some_and(|p| p.eq_ignore_ascii_case(tag))
}

pub fn parse_m3u(text: &str, source_id: &str, location: &str) -> Result<Vec<LiveChannel>, String> {
    let text = text.trim_start_matches('\u{feff}');
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if !lines
        .next()
        .map(|l| l.to_ascii_uppercase().starts_with("#EXTM3U"))
        .unwrap_or(false)
    {
        return Err("This is not an M3U playlist.".into());
    }

    let upper = text.to_ascii_uppercase();
    if upper.contains("#EXT-X-TARGETDURATION") || upper.contains("#EXT-X-STREAM-INF") {
        // A single HLS stream rather than a channel list: expose it as one channel.
        return match remote_url(location) {
            Some(url) => Ok(vec![LiveChannel {
                source_id: source_id.to_string(),
                name: url.path_segments().and_then(|s| s.last()).unwrap_or("Live stream").to_string(),
                url: url.to_string(),
                group: uncategorized(),
                logo: String::new(),
                tvg_id: String::new(),
                number: String::new(),
            }]),
            None => Err("This is a single HLS stream, not a channel playlist.".into()),
        };
    }

    let base = remote_url(location);
    let mut attributes: Vec<(String, String)> = Vec::new();
    let mut title = String::new();
    let mut extgrp = String::new();
    let mut seen = HashSet::new();
    let mut found = Vec::new();

    for line in lines {
        if has_tag(line, "#EXTINF:") {
            let comma = metadata_comma(line);
            let head = comma.map_or(line, |c| &line[..c]);
            attributes = attribute_regex()
                .captures_iter(head)
                .map(|c| {
                    let value = c.get(2).or(c.get(3)).or(c.get(4)).map_or("", |m| m.as_str());
                    (c[1].to_ascii_lowercase(), value.trim().to_string())
                })
                .collect();
            title = comma.map_or(String::new(), |c| line[c + 1..].trim().to_string());
        } else if has_tag(line, "#EXTGRP:") {
            extgrp = line[8..].trim().to_string();
        } else if !line.starts_with('#') {
            // Kodi-style "url|User-Agent=..." suffixes aren't understood by mpv.
            let target = line.split('|').next().unwrap_or(line).trim();
            let resolved = Url::parse(target)
                .ok()
                .or_else(|| base.as_ref().and_then(|b| b.join(target).ok()));
            if let Some(url) = resolved.filter(|u| STREAM_SCHEMES.contains(&u.scheme())) {
                let url = url.to_string();
                if seen.insert(url.clone()) {
                    let attr = |key: &str| {
                        attributes
                            .iter()
                            .find(|(k, v)| k == key && !v.is_empty())
                            .map(|(_, v)| v.clone())
                    };
                    let name = if !title.is_empty() {
                        title.clone()
                    } else {
                        attr("tvg-name").unwrap_or_else(|| format!("Channel {}", found.len() + 1))
                    };
                    let group = attr("group-title")
                        .or_else(|| (!extgrp.is_empty()).then(|| extgrp.clone()))
                        .unwrap_or_else(uncategorized);
                    found.push(LiveChannel {
                        source_id: source_id.to_string(),
                        name,
                        url,
                        group,
                        logo: attr("tvg-logo").unwrap_or_default(),
                        tvg_id: attr("tvg-id").unwrap_or_default(),
                        number: attr("tvg-chno").unwrap_or_default(),
                    });
                }
            }
            attributes.clear();
            title.clear();
            extgrp.clear();
        }
    }

    if found.is_empty() {
        return Err("The playlist has no playable channels.".into());
    }
    Ok(found)
}

async fn fetch_channels(source: &ChannelSource) -> Result<Vec<LiveChannel>, String> {
    let text = read_playlist(&source.location)
        .await
        .map_err(|e| format!("Could not load {}: {}", source.name, e))?;
    parse_m3u(&text, &source.id, &source.location)
        .map_err(|e| format!("Could not load {}: {}", source.name, e))
}

#[tauri::command]
pub async fn get_live_channels(app_handle: AppHandle) -> Result<ChannelData, String> {
    let path = data_path(&app_handle)?;
    let _guard = store_lock().lock().await;
    load(&path)
}

#[tauri::command]
pub async fn add_live_source(
    app_handle: AppHandle,
    name: String,
    location: String,
) -> Result<ChannelData, String> {
    let location = location.trim().to_string();
    if remote_url(&location).is_none() && !local_path(&location).is_file() {
        return Err("Enter a playlist URL or choose an existing M3U file.".into());
    }
    let path = data_path(&app_handle)?;
    if load(&path)?.sources.iter().any(|s| s.location == location) {
        return Err("This playlist is already added.".into());
    }

    let name = name.trim();
    let mut source = ChannelSource {
        id: new_id(),
        name: if name.is_empty() { "Live playlist".into() } else { name.into() },
        location,
        updated_at: None,
    };
    let channels = fetch_channels(&source).await?;
    source.updated_at = Some(chrono::Utc::now().timestamp());

    let _guard = store_lock().lock().await;
    let mut data = load(&path)?;
    if data.sources.iter().any(|s| s.location == source.location) {
        return Err("This playlist is already added.".into());
    }
    data.sources.push(source);
    data.channels.extend(channels);
    save(&path, &data)?;
    Ok(data)
}

#[tauri::command]
pub async fn refresh_live_source(app_handle: AppHandle, id: String) -> Result<ChannelData, String> {
    let path = data_path(&app_handle)?;
    let source = load(&path)?
        .sources
        .into_iter()
        .find(|s| s.id == id)
        .ok_or("This playlist was removed.")?;
    let channels = fetch_channels(&source).await?;

    let _guard = store_lock().lock().await;
    let mut data = load(&path)?;
    let Some(stored) = data.sources.iter_mut().find(|s| s.id == id) else {
        return Ok(data);
    };
    stored.updated_at = Some(chrono::Utc::now().timestamp());
    data.channels.retain(|c| c.source_id != id);
    data.channels.extend(channels);
    save(&path, &data)?;
    Ok(data)
}

#[tauri::command]
pub async fn remove_live_source(app_handle: AppHandle, id: String) -> Result<ChannelData, String> {
    let path = data_path(&app_handle)?;
    let _guard = store_lock().lock().await;
    let mut data = load(&path)?;
    data.sources.retain(|s| s.id != id);
    data.channels.retain(|c| c.source_id != id);
    save(&path, &data)?;
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "https://example.com/lists/tv.m3u";

    #[test]
    fn parses_attributes_titles_and_groups() {
        let text = "\u{feff}#EXTM3U\n\
            #EXTINF:-1 tvg-id=\"a.us\" tvg-chno=\"101\" tvg-logo='https://x/logo.png' group-title=\"News, Weather\",CBS News\n\
            https://cdn.example.com/cbs.m3u8\n\
            #EXTINF:-1 tvg-name=Fallback,\n\
            relative/stream.m3u8\n\
            #EXTINF:-1,Grouped\n\
            #EXTGRP:Movies\n\
            https://cdn.example.com/m.m3u8|User-Agent=Foo\n";
        let channels = parse_m3u(text, "s", BASE).unwrap();
        assert_eq!(channels.len(), 3);
        assert_eq!(channels[0].name, "CBS News");
        assert_eq!(channels[0].group, "News, Weather");
        assert_eq!(channels[0].number, "101");
        assert_eq!(channels[0].logo, "https://x/logo.png");
        assert_eq!(channels[0].tvg_id, "a.us");
        assert_eq!(channels[1].name, "Fallback");
        assert_eq!(channels[1].url, "https://example.com/lists/relative/stream.m3u8");
        assert_eq!(channels[1].group, "Uncategorized");
        assert_eq!(channels[2].group, "Movies");
        assert_eq!(channels[2].url, "https://cdn.example.com/m.m3u8");
    }

    #[test]
    fn skips_duplicates_and_unsupported_schemes() {
        let text = "#EXTM3U\n#EXTINF:-1,A\nhttps://a/1\n#EXTINF:-1,B\nhttps://a/1\n#EXTINF:-1,C\nftp://a/2\n";
        let channels = parse_m3u(text, "s", BASE).unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].name, "A");
    }

    #[test]
    fn rejects_non_playlists() {
        assert!(parse_m3u("<html></html>", "s", BASE).is_err());
        assert!(parse_m3u("#EXTM3U\n# nothing\n", "s", BASE).is_err());
    }

    #[test]
    fn treats_hls_media_playlist_as_single_channel() {
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXTINF:6,\nseg1.ts\n";
        let channels = parse_m3u(text, "s", "https://cdn.example.com/live/news.m3u8").unwrap();
        assert_eq!(channels.len(), 1);
        assert_eq!(channels[0].url, "https://cdn.example.com/live/news.m3u8");
        assert_eq!(channels[0].name, "news.m3u8");
        assert!(parse_m3u(text, "s", "C:\\local.m3u8").is_err());
    }
}
