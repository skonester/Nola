//! Live TV: M3U channel playlists (Pluto TV, Plex, Samsung TV Plus, Roku, Tubi, or
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
use tauri::Emitter;
use crate::live_epg::{self, Programme};

const MAX_PLAYLIST_BYTES: usize = 32 * 1024 * 1024;
const MAX_GUIDE_DOWNLOAD_BYTES: usize = 64 * 1024 * 1024;
const PLAYLIST_TTL: i64 = 24 * 60 * 60;
const GUIDE_TTL: i64 = 4 * 60 * 60;
const RETRY_INTERVAL: i64 = 15 * 60;
const STREAM_SCHEMES: &[&str] = &["http", "https", "rtsp", "rtmp", "udp", "rtp"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelSource {
    pub id: String,
    pub name: String,
    pub location: String,
    #[serde(default)]
    pub updated_at: Option<i64>,
    #[serde(default)]
    pub checked_at: Option<i64>,
    /// None distinguishes old sources whose playlist header has not been read.
    #[serde(default)]
    pub epg_urls: Option<Vec<String>>,
    #[serde(default)]
    pub epg_url: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuideSource {
    pub source_id: String,
    pub urls: Vec<String>,
    pub updated_at: Option<i64>,
    pub checked_at: Option<i64>,
    pub error: Option<String>,
    pub programmes: Vec<Programme>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct GuideData {
    pub sources: Vec<GuideSource>,
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

fn save<T: Serialize>(path: &PathBuf, data: &T) -> Result<(), String> {
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

async fn read_resource(location: &str, limit: usize) -> Result<Vec<u8>, String> {
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
            if body.len() > limit {
                return Err(format!("Source exceeds {} MB", limit / 1024 / 1024));
            }
        }
        body
    } else {
        let path = local_path(location);
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|_| "Source file not found".to_string())?;
        if meta.len() as usize > limit {
            return Err(format!("Source exceeds {} MB", limit / 1024 / 1024));
        }
        // Limit the read too, in case the file grows after the metadata check.
        use tokio::io::AsyncReadExt;
        let file = tokio::fs::File::open(&path).await.map_err(|e| e.to_string())?;
        let mut body = Vec::new();
        file.take(limit as u64 + 1).read_to_end(&mut body).await.map_err(|e| e.to_string())?;
        if body.len() > limit {
            return Err(format!("Source exceeds {} MB", limit / 1024 / 1024));
        }
        body
    };
    Ok(bytes)
}

fn guide_path(path: &PathBuf) -> PathBuf {
    path.with_file_name("live_guide.json")
}

fn load_guide(path: &PathBuf) -> GuideData {
    std::fs::read_to_string(guide_path(path)).ok()
        .and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

fn epg_urls(text: &str, location: &str) -> Vec<String> {
    let header = text.trim_start_matches('\u{feff}').lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let base = remote_url(location).or_else(|| {
        local_path(location).canonicalize().ok().and_then(|p| Url::from_file_path(p).ok())
    });
    let mut urls = Vec::new();
    for attr in attribute_regex().captures_iter(header) {
        if !matches!(attr[1].to_ascii_lowercase().as_str(), "url-tvg" | "x-tvg-url") {
            continue;
        }
        let value = attr.get(2).or(attr.get(3)).or(attr.get(4)).map_or("", |v| v.as_str());
        for entry in value.split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()) {
            if let Some(url) = Url::parse(entry).ok().or_else(|| base.as_ref().and_then(|b| b.join(entry).ok())) {
                if matches!(url.scheme(), "http" | "https" | "file") && !urls.contains(&url.to_string()) {
                    urls.push(url.to_string());
                }
            }
        }
    }
    urls.truncate(8);
    urls
}

fn effective_epg_urls(source: &ChannelSource) -> Vec<String> {
    source.epg_url.as_ref().map(|url| vec![url.clone()])
        .unwrap_or_else(|| source.epg_urls.clone().unwrap_or_default())
}

fn refresh_due(updated: Option<i64>, checked: Option<i64>, ttl: i64, now: i64) -> bool {
    updated.is_none_or(|t| now - t >= ttl || t > now + 3600)
        && checked.is_none_or(|t| now - t >= RETRY_INTERVAL || t > now + 3600)
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

async fn fetch_channels(source: &ChannelSource) -> Result<(Vec<LiveChannel>, Vec<String>), String> {
    let bytes = read_resource(&source.location, MAX_PLAYLIST_BYTES)
        .await
        .map_err(|e| format!("Could not load {}: {}", source.name, e))?;
    let text = String::from_utf8_lossy(&bytes);
    let channels = parse_m3u(&text, &source.id, &source.location)
        .map_err(|e| format!("Could not load {}: {}", source.name, e))?;
    Ok((channels, epg_urls(&text, &source.location)))
}

async fn refresh_guide(path: &PathBuf, source: &ChannelSource, channels: &[LiveChannel], force: bool) -> Result<(), String> {
    let mut data = load_guide(path);
    let urls = effective_epg_urls(source);
    let mut guide = data.sources.iter().find(|s| s.source_id == source.id).cloned()
        .unwrap_or_else(|| GuideSource { source_id: source.id.clone(), ..Default::default() });
    let now = chrono::Utc::now().timestamp();
    if urls.is_empty() {
        data.sources.retain(|s| s.source_id != source.id);
        return save(&guide_path(path), &data);
    }
    if !force && urls == guide.urls && !refresh_due(guide.updated_at, guide.checked_at, GUIDE_TTL, now) {
        return Ok(());
    }
    let channel_ids: HashSet<String> = channels.iter().filter(|c| c.source_id == source.id)
        .map(|c| c.tvg_id.clone()).filter(|id| !id.is_empty()).collect();
    let mut programmes = Vec::new();
    let mut failure = None;
    for url in &urls {
        let result = async {
            let bytes = read_resource(url, MAX_GUIDE_DOWNLOAD_BYTES).await?;
            let ids = channel_ids.clone();
            tokio::task::spawn_blocking(move || {
                live_epg::parse(&live_epg::decode(&bytes)?, &ids, now)
            }).await.map_err(|e| e.to_string())?
        }.await;
        match result {
            Ok(mut parsed) => programmes.append(&mut parsed),
            Err(error) => { failure = Some(error); break; }
        }
    }
    guide.checked_at = Some(now);
    guide.urls = urls;
    if let Some(error) = failure {
        // A failed download must not erase the last usable guide.
        guide.error = Some(format!("Could not refresh guide: {error}"));
    } else {
        programmes.sort_by(|a, b| (&a.channel_id, a.start, a.stop).cmp(&(&b.channel_id, b.start, b.stop)));
        programmes.dedup_by(|a, b| a.channel_id == b.channel_id && a.start == b.start && a.stop == b.stop);
        guide.programmes = programmes;
        guide.updated_at = Some(now);
        guide.error = None;
    }
    data.sources.retain(|s| s.source_id != source.id);
    data.sources.push(guide);
    save(&guide_path(path), &data)
}

/// The caller holds store_lock throughout the refresh. Readers use atomic
/// snapshots, so a slow network never prevents browsing cached channels.
async fn refresh_source(path: &PathBuf, id: &str, force: bool) -> Result<ChannelData, String> {
    let mut data = load(path)?;
    let Some(index) = data.sources.iter().position(|s| s.id == id) else { return Ok(data); };
    let source = &data.sources[index];
    let now = chrono::Utc::now().timestamp();
    let ttl = if source.epg_urls.is_none() { 0 } else { PLAYLIST_TTL };
    if force || refresh_due(source.updated_at, source.checked_at, ttl, now) {
        let fetched = fetch_channels(source).await;
        let source = &mut data.sources[index];
        source.checked_at = Some(now);
        match fetched {
            Ok((channels, urls)) => {
                source.updated_at = Some(now);
                source.epg_urls = Some(urls);
                source.error = None;
                data.channels.retain(|c| c.source_id != id);
                data.channels.extend(channels);
            }
            Err(error) => source.error = Some(error),
        }
        save(path, &data)?;
    }
    refresh_guide(path, &data.sources[index], &data.channels, force).await?;
    Ok(data)
}

/// Refresh due sources at startup and every fifteen minutes while Nola runs.
/// Retry failed requests after fifteen minutes; keep both caches on failure.
pub fn start_refresh(app_handle: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            if let Err(error) = refresh_live_data(app_handle.clone()).await {
                log::warn!("[live-tv] background refresh: {error}");
            }
            tokio::time::sleep(std::time::Duration::from_secs(RETRY_INTERVAL as u64)).await;
        }
    });
}

#[tauri::command]
pub async fn refresh_live_data(app_handle: AppHandle) -> Result<(), String> {
    let path = data_path(&app_handle)?;
    let sources = load(&path)?.sources;
    for source in sources {
        let _guard = store_lock().lock().await;
        refresh_source(&path, &source.id, false).await?;
    }
    app_handle.emit("live-tv-updated", ()).ok();
    Ok(())
}

#[tauri::command]
pub async fn get_live_guide(app_handle: AppHandle) -> Result<GuideData, String> {
    Ok(load_guide(&data_path(&app_handle)?))
}

#[tauri::command]
pub async fn get_live_programmes(app_handle: AppHandle, source_id: String, tvg_id: String) -> Result<Vec<Programme>, String> {
    let path = data_path(&app_handle)?;
    tokio::task::spawn_blocking(move || {
        load_guide(&path).sources.into_iter().find(|s| s.source_id == source_id)
            .map(|s| s.programmes.into_iter().filter(|p| p.channel_id == tvg_id).collect())
            .unwrap_or_default()
    }).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_live_channels(app_handle: AppHandle) -> Result<ChannelData, String> {
    let path = data_path(&app_handle)?;
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
    let _guard = store_lock().lock().await;
    if load(&path)?.sources.iter().any(|s| s.location == location) {
        return Err("This playlist is already added.".into());
    }

    let name = name.trim();
    let mut source = ChannelSource {
        id: new_id(),
        name: if name.is_empty() { "Live playlist".into() } else { name.into() },
        location,
        updated_at: None,
        checked_at: None,
        epg_urls: None,
        epg_url: None,
        error: None,
    };
    let (channels, urls) = fetch_channels(&source).await?;
    source.updated_at = Some(chrono::Utc::now().timestamp());
    source.checked_at = source.updated_at;
    source.epg_urls = Some(urls);

    let mut data = load(&path)?;
    if data.sources.iter().any(|s| s.location == source.location) {
        return Err("This playlist is already added.".into());
    }
    data.sources.push(source.clone());
    data.channels.extend(channels);
    save(&path, &data)?;
    refresh_guide(&path, &source, &data.channels, true).await?;
    app_handle.emit("live-tv-updated", ()).ok();
    Ok(data)
}

#[tauri::command]
pub async fn refresh_live_source(app_handle: AppHandle, id: String) -> Result<ChannelData, String> {
    let path = data_path(&app_handle)?;
    let _guard = store_lock().lock().await;
    let data = refresh_source(&path, &id, true).await?;
    app_handle.emit("live-tv-updated", ()).ok();
    Ok(data)
}

#[tauri::command]
pub async fn set_live_epg_source(app_handle: AppHandle, id: String, location: String) -> Result<ChannelData, String> {
    let location = location.trim();
    if !location.is_empty() && remote_url(location).is_none() && !local_path(location).is_file() {
        return Err("Enter an XMLTV feed URL or an existing XMLTV file path.".into());
    }
    let path = data_path(&app_handle)?;
    let _guard = store_lock().lock().await;
    let mut data = load(&path)?;
    let source = data.sources.iter_mut().find(|s| s.id == id).ok_or("This playlist was removed.")?;
    source.epg_url = (!location.is_empty()).then(|| location.to_string());
    let source = source.clone();
    save(&path, &data)?;
    refresh_guide(&path, &source, &data.channels, true).await?;
    app_handle.emit("live-tv-updated", ()).ok();
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
    let mut guide = load_guide(&path);
    guide.sources.retain(|s| s.source_id != id);
    save(&guide_path(&path), &guide)?;
    app_handle.emit("live-tv-updated", ()).ok();
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

    #[test]
    fn discovers_relative_and_multiple_xmltv_feeds() {
        let text = "\u{feff}#EXTM3U URL-TVG=\"guide.xml, https://epg.example/all.xml.gz\" x-tvg-url='guide.xml'\n";
        assert_eq!(epg_urls(text, BASE), vec!["https://example.com/lists/guide.xml", "https://epg.example/all.xml.gz"]);
        assert!(epg_urls("#EXTM3U url-tvg=\"ftp://a/x.xml\"", BASE).is_empty());
    }

    #[test]
    fn refresh_policy_handles_fresh_stale_failed_and_legacy_sources() {
        let now = 100_000;
        assert!(!refresh_due(Some(now - 100), Some(now - 100), PLAYLIST_TTL, now));
        assert!(refresh_due(Some(now - PLAYLIST_TTL), Some(now - PLAYLIST_TTL), PLAYLIST_TTL, now));
        assert!(!refresh_due(None, Some(now - 10), PLAYLIST_TTL, now));
        assert!(refresh_due(None, Some(now - RETRY_INTERVAL), PLAYLIST_TTL, now));
        let old: ChannelSource = serde_json::from_str(r#"{"id":"old","name":"Old","location":"https://a/list.m3u","updated_at":99999}"#).unwrap();
        assert!(old.epg_urls.is_none());
        assert!(refresh_due(old.updated_at, old.checked_at, 0, now));
    }

    #[tokio::test]
    async fn offline_refresh_keeps_channels_and_guide_and_throttles_retries() {
        let directory = std::env::temp_dir().join(format!("nola-live-test-{}", new_id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("live_channels.json");
        let source = ChannelSource {
            id: "s".into(), name: "Saved".into(), location: directory.join("missing.m3u").to_string_lossy().into_owned(),
            updated_at: Some(1), checked_at: None, epg_urls: Some(vec![]),
            epg_url: Some(directory.join("missing.xml").to_string_lossy().into_owned()), error: None,
        };
        let channels = parse_m3u("#EXTM3U\n#EXTINF:-1 tvg-id=\"a\",News\nhttps://example.com/live\n", "s", BASE).unwrap();
        save(&path, &ChannelData { sources: vec![source], channels: channels.clone() }).unwrap();
        let programme = Programme { channel_id: "a".into(), start: 1, stop: 2, title: "Saved show".into(), subtitle: String::new(), description: String::new() };
        save(&guide_path(&path), &GuideData { sources: vec![GuideSource {
            source_id: "s".into(), programmes: vec![programme.clone()], updated_at: Some(1), ..Default::default()
        }] }).unwrap();
        let data = refresh_source(&path, "s", false).await.unwrap();
        assert_eq!(data.channels[0].url, channels[0].url);
        assert_eq!(data.sources[0].updated_at, Some(1));
        assert!(data.sources[0].error.is_some());
        let guide = load_guide(&path);
        assert_eq!(guide.sources[0].programmes, vec![programme]);
        assert_eq!(guide.sources[0].updated_at, Some(1));
        assert!(guide.sources[0].error.is_some());
        let checked = data.sources[0].checked_at;
        let second = refresh_source(&path, "s", false).await.unwrap();
        assert_eq!(second.sources[0].checked_at, checked);
        assert_eq!(load_guide(&path).sources[0].checked_at, guide.sources[0].checked_at);
        std::fs::remove_file(guide_path(&path)).unwrap();
        std::fs::remove_file(&path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[tokio::test]
    async fn refresh_migrates_legacy_sources_and_loads_relative_gzip_guides() {
        use std::io::Write;
        let directory = std::env::temp_dir().join(format!("nola-live-test-{}", new_id()));
        std::fs::create_dir(&directory).unwrap();
        let path = directory.join("live_channels.json");
        let playlist_path = directory.join("channels.m3u");
        let xml_path = directory.join("guide.xml.gz");
        std::fs::write(&playlist_path, "#EXTM3U url-tvg=\"guide.xml.gz\"\n#EXTINF:-1 tvg-id=\"news\",News\nhttps://example.com/live\n").unwrap();
        let now = chrono::Utc::now();
        let start = (now - chrono::Duration::minutes(10)).format("%Y%m%d%H%M%S +0000");
        let stop = (now + chrono::Duration::minutes(50)).format("%Y%m%d%H%M%S +0000");
        let xml = format!("<tv><programme channel=\"news\" start=\"{start}\" stop=\"{stop}\"><title>Live news</title></programme></tv>");
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(xml.as_bytes()).unwrap();
        std::fs::write(&xml_path, gzip.finish().unwrap()).unwrap();
        let source: ChannelSource = serde_json::from_value(serde_json::json!({
            "id": "s", "name": "Local", "location": playlist_path, "updated_at": now.timestamp()
        })).unwrap();
        save(&path, &ChannelData { sources: vec![source], channels: vec![] }).unwrap();
        let data = refresh_source(&path, "s", false).await.unwrap();
        assert_eq!(data.channels.len(), 1);
        assert!(data.sources[0].epg_urls.as_ref().unwrap()[0].starts_with("file://"));
        let guide = load_guide(&path);
        assert_eq!(guide.sources[0].programmes[0].title, "Live news");
        assert!(guide.sources[0].error.is_none());
        for file in [xml_path, playlist_path, guide_path(&path), path] { std::fs::remove_file(file).unwrap(); }
        std::fs::remove_dir(directory).unwrap();
    }
}
