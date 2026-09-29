//! Local HLS proxy for Live TV.
//!
//! FAST services (Pluto TV, Roku, Samsung TV Plus, ...) stitch ads into their
//! HLS streams. Each ad comes from a different packager, so at every
//! `#EXT-X-DISCONTINUITY` the MPEG-TS program can move to new PIDs. VLC's HLS
//! module rebuilds its demuxer per discontinuity and doesn't care; FFmpeg's HLS
//! demuxer (used by mpv) ignores the tag, exposes the ad as brand-new tracks
//! that mpv never selects, and playback freezes until the content returns.
//!
//! This proxy sits between mpv and the CDN and fixes the stream up the way VLC
//! effectively does:
//! - the master playlist is reduced to one variant, so FFmpeg doesn't download
//!   a segment of every bitrate before starting (what made tuning in slow);
//! - AES-128 segments are decrypted here, and every segment's PAT/PMT is
//!   rewritten to one fixed PID layout, so ads land on the tracks mpv is
//!   already playing and mpv only sees a timestamp reset, which it handles.
//!
//! Anything the proxy doesn't understand (fMP4, SAMPLE-AES, byte ranges,
//! non-HLS URLs) is passed through unchanged.

use aes::cipher::{block_padding::NoPadding, BlockDecryptMut, KeyIvInit};
use axum::{
    extract::Path,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use tokio::sync::OnceCell;
use url::Url;

const PLAYLIST_TYPE: &str = "application/vnd.apple.mpegurl";
const MAX_MASTERS: usize = 8;
const KEY_TTL: Duration = Duration::from_secs(300);

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

static BASE_URL: OnceCell<String> = OnceCell::const_new();

fn client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent("Mozilla/5.0")
            .build()
            .expect("failed to build live proxy HTTP client")
    })
}

/// Rewritten master playlists, served by id (newest last).
fn masters() -> &'static Mutex<VecDeque<(String, String)>> {
    static MASTERS: OnceLock<Mutex<VecDeque<(String, String)>>> = OnceLock::new();
    MASTERS.get_or_init(|| Mutex::new(VecDeque::new()))
}

fn keys() -> &'static Mutex<HashMap<String, (Instant, [u8; 16])>> {
    static KEYS: OnceLock<Mutex<HashMap<String, (Instant, [u8; 16])>>> = OnceLock::new();
    KEYS.get_or_init(|| Mutex::new(HashMap::new()))
}

async fn base_url() -> Result<&'static String, String> {
    BASE_URL
        .get_or_try_init(|| async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .map_err(|e| format!("live proxy: {e}"))?;
            let addr = listener.local_addr().map_err(|e| e.to_string())?;
            let app = Router::new()
                .route("/live/master/{file}", get(serve_master))
                .route("/live/media/{file}", get(serve_media))
                .route("/live/segment/{file}", get(serve_segment));
            tokio::spawn(async move {
                axum::serve(listener, app).await.ok();
            });
            log::info!("[live-proxy] listening on {addr}");
            Ok(format!("http://{addr}"))
        })
        .await
}

fn encode(value: &str) -> String {
    URL_SAFE_NO_PAD.encode(value)
}

/// Decode a `<token>.<ext>` path component.
fn decode(file: &str) -> Option<String> {
    let token = file.split_once('.').map_or(file, |(t, _)| t);
    String::from_utf8(URL_SAFE_NO_PAD.decode(token).ok()?).ok()
}

/// Returns the URL mpv should open for a live channel: a proxied playlist for
/// HLS streams, or the original URL for anything else (or if the upstream
/// can't be reached — mpv then reports the error as before).
#[tauri::command]
pub async fn live_stream_url(url: String) -> Result<String, String> {
    match proxied_url(&url).await {
        Ok(Some(proxied)) => Ok(proxied),
        Ok(None) => Ok(url),
        Err(e) => {
            log::warn!("[live-proxy] not proxying {url}: {e}");
            Ok(url)
        }
    }
}

async fn proxied_url(url: &str) -> Result<Option<String>, String> {
    let parsed = Url::parse(url).map_err(|e| e.to_string())?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Ok(None);
    }
    let base = base_url().await?;
    let mut response = client()
        .get(parsed)
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| e.to_string())?;
    let final_url = response.url().clone();
    // A continuous stream (e.g. raw MPEG-TS) never ends: only look at the start.
    let mut head = Vec::new();
    while head.len() < 7 {
        match response.chunk().await.map_err(|e| e.to_string())? {
            Some(chunk) => head.extend_from_slice(&chunk),
            None => break,
        }
    }
    let start = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&head);
    if !start.trim_ascii_start().starts_with(b"#EXTM3U") {
        return Ok(None);
    }
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        head.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&head);

    if !text.contains("#EXT-X-STREAM-INF") {
        return Ok(Some(format!("{base}/live/media/{}.m3u8", encode(final_url.as_str()))));
    }
    let Some(master) = rewrite_master(&text, &final_url, base) else {
        return Ok(None);
    };
    let id = format!("{:x}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default());
    let mut stored = masters().lock().map_err(|e| e.to_string())?;
    stored.push_back((id.clone(), master));
    while stored.len() > MAX_MASTERS {
        stored.pop_front();
    }
    Ok(Some(format!("{base}/live/master/{id}.m3u8")))
}

fn playlist(body: String) -> Response {
    ([(header::CONTENT_TYPE, PLAYLIST_TYPE)], body).into_response()
}

fn upstream_error(what: &str, error: impl std::fmt::Display) -> Response {
    log::warn!("[live-proxy] {what}: {error}");
    (StatusCode::BAD_GATEWAY, error.to_string()).into_response()
}

async fn serve_master(Path(file): Path<String>) -> Response {
    let id = file.trim_end_matches(".m3u8");
    let found = masters()
        .lock()
        .ok()
        .and_then(|m| m.iter().find(|(k, _)| k == id).map(|(_, v)| v.clone()));
    match found {
        Some(body) => playlist(body),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn serve_media(Path(file): Path<String>) -> Response {
    let Some(url) = decode(&file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let result = async {
        let response = client().get(&url).send().await?.error_for_status()?;
        let final_url = response.url().clone();
        Ok::<_, reqwest::Error>((final_url, response.text().await?))
    }
    .await;
    let (final_url, text) = match result {
        Ok(r) => r,
        Err(e) => return upstream_error("media playlist", e),
    };
    // Set before the server started accepting requests.
    let base = BASE_URL.get().map_or("", String::as_str);
    playlist(rewrite_media(&text, &final_url, base))
}

async fn serve_segment(Path(file): Path<String>) -> Response {
    let Some(token) = decode(&file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut parts = token.splitn(3, '\n');
    let (url, key_url, iv) = (
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
        parts.next().unwrap_or_default(),
    );
    let mut data = match fetch_bytes(url).await {
        Ok(d) => d,
        Err(e) => return upstream_error("segment", e),
    };
    if !key_url.is_empty() {
        let key = match fetch_key(key_url).await {
            Ok(k) => k,
            Err(e) => return upstream_error("key", e),
        };
        data = match decrypt(data, &key, iv) {
            Some(d) => d,
            None => return upstream_error("segment", "could not decrypt"),
        };
    }
    if let Some(normalized) = normalize_ts(&data) {
        data = normalized;
    }
    ([(header::CONTENT_TYPE, "video/mp2t")], data).into_response()
}

async fn fetch_bytes(url: &str) -> Result<Vec<u8>, reqwest::Error> {
    let response = client().get(url).send().await?.error_for_status()?;
    Ok(response.bytes().await?.to_vec())
}

async fn fetch_key(url: &str) -> Result<[u8; 16], String> {
    if let Some((at, key)) = keys().lock().ok().and_then(|k| k.get(url).copied()) {
        if at.elapsed() < KEY_TTL {
            return Ok(key);
        }
    }
    let bytes = fetch_bytes(url).await.map_err(|e| e.to_string())?;
    let key: [u8; 16] = bytes
        .get(..16)
        .and_then(|b| b.try_into().ok())
        .ok_or("key is not 16 bytes")?;
    if let Ok(mut cache) = keys().lock() {
        cache.retain(|_, (at, _)| at.elapsed() < KEY_TTL);
        cache.insert(url.to_string(), (Instant::now(), key));
    }
    Ok(key)
}

fn decrypt(mut data: Vec<u8>, key: &[u8; 16], iv_hex: &str) -> Option<Vec<u8>> {
    let iv = u128::from_str_radix(iv_hex, 16).ok()?.to_be_bytes();
    Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_mut::<NoPadding>(&mut data)
        .ok()?;
    // Strip PKCS#7 padding; some packagers don't pad, so keep the data as-is
    // when the tail isn't valid padding.
    if let Some(&pad) = data.last() {
        let pad = pad as usize;
        if (1..=16).contains(&pad) && data[data.len() - pad..].iter().all(|&b| b as usize == pad) {
            data.truncate(data.len() - pad);
        }
    }
    Some(data)
}

// ── playlist rewriting ──────────────────────────────────────────────────────

/// Value of `NAME=` in an attribute list, without quotes.
fn attribute<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let mut rest = line.split_once(':')?.1;
    while !rest.is_empty() {
        let (key, after) = rest.split_once('=')?;
        let (value, next) = if let Some(quoted) = after.strip_prefix('"') {
            let end = quoted.find('"')?;
            (&quoted[..end], quoted[end + 1..].trim_start_matches(','))
        } else {
            after.split_once(',').unwrap_or((after, ""))
        };
        if key.trim() == name {
            return Some(value);
        }
        rest = next;
    }
    None
}

fn absolute(base: &Url, reference: &str) -> String {
    base.join(reference.trim())
        .map(|u| u.to_string())
        .unwrap_or_else(|_| reference.trim().to_string())
}

/// Rewrite `URI="..."` in a tag line.
fn with_uri(line: &str, uri: &str) -> String {
    match line.find("URI=\"") {
        Some(start) => {
            let value_start = start + 5;
            let end = line[value_start..].find('"').map_or(line.len(), |e| value_start + e);
            format!("{}{}{}", &line[..value_start], uri, &line[end..])
        }
        None => line.to_string(),
    }
}

/// Reduce a master playlist to its highest-bandwidth variant (the same pick
/// as mpv's default `hls-bitrate=max`) plus the renditions it references.
fn rewrite_master(text: &str, base: &Url, proxy: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut best: Option<(u64, &str, &str)> = None;
    for (i, line) in lines.iter().enumerate() {
        if !line.starts_with("#EXT-X-STREAM-INF") {
            continue;
        }
        let Some(uri) = lines[i + 1..].iter().find(|l| !l.is_empty() && !l.starts_with('#')) else {
            continue;
        };
        let bandwidth = attribute(line, "BANDWIDTH").and_then(|b| b.parse().ok()).unwrap_or(0);
        if best.is_none_or(|(b, _, _)| bandwidth > b) {
            best = Some((bandwidth, line, uri));
        }
    }
    let (_, stream_inf, uri) = best?;

    let mut out = vec!["#EXTM3U".to_string()];
    for line in &lines {
        if line.starts_with("#EXT-X-VERSION") || line.starts_with("#EXT-X-INDEPENDENT-SEGMENTS") {
            out.push(line.to_string());
        } else if line.starts_with("#EXT-X-MEDIA:") {
            let (Some(kind), Some(group)) = (attribute(line, "TYPE"), attribute(line, "GROUP-ID")) else {
                continue;
            };
            if attribute(stream_inf, kind) != Some(group) {
                continue;
            }
            match attribute(line, "URI") {
                Some(rendition) if kind == "AUDIO" || kind == "VIDEO" => {
                    let url = absolute(base, rendition);
                    out.push(with_uri(line, &format!("{proxy}/live/media/{}.m3u8", encode(&url))));
                }
                Some(rendition) => out.push(with_uri(line, &absolute(base, rendition))),
                None => out.push(line.to_string()),
            }
        }
    }
    out.push(stream_inf.to_string());
    out.push(format!("{proxy}/live/media/{}.m3u8", encode(&absolute(base, uri))));
    Some(out.join("\n") + "\n")
}

/// Point every segment at the proxy, carrying its AES-128 key and IV, and
/// drop the `#EXT-X-KEY` tags since the proxy serves decrypted segments.
fn rewrite_media(text: &str, base: &Url, proxy: &str) -> String {
    let unsupported = text.lines().any(|l| {
        l.starts_with("#EXT-X-MAP")
            || l.starts_with("#EXT-X-BYTERANGE")
            || (l.starts_with("#EXT-X-KEY")
                && !matches!(attribute(l, "METHOD"), Some("NONE" | "AES-128")))
    });

    let mut sequence: u128 = text
        .lines()
        .find_map(|l| l.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let mut key: Option<(String, Option<String>)> = None;
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() {
            continue;
        }
        if unsupported {
            // Leave it to FFmpeg; only resolve relative URIs against upstream.
            out.push(if line.starts_with('#') {
                attribute(line, "URI").map_or(line.to_string(), |u| with_uri(line, &absolute(base, u)))
            } else {
                absolute(base, line)
            });
            continue;
        }
        if line.starts_with("#EXT-X-KEY") {
            key = match (attribute(line, "METHOD"), attribute(line, "URI")) {
                (Some("AES-128"), Some(uri)) => Some((
                    absolute(base, uri),
                    attribute(line, "IV").map(|iv| iv.trim_start_matches("0x").trim_start_matches("0X").to_string()),
                )),
                _ => None,
            };
        } else if line.starts_with('#') {
            out.push(line.to_string());
        } else {
            let url = absolute(base, line);
            let (key_url, iv) = match &key {
                Some((k, Some(iv))) => (k.as_str(), iv.clone()),
                Some((k, None)) => (k.as_str(), format!("{sequence:032x}")),
                None => ("", String::new()),
            };
            let extension = Url::parse(&url)
                .ok()
                .and_then(|u| u.path().rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()))
                .filter(|e| e.len() <= 4 && e.chars().all(|c| c.is_ascii_alphanumeric()))
                .unwrap_or_else(|| "ts".into());
            let token = encode(&format!("{url}\n{key_url}\n{iv}"));
            out.push(format!("{proxy}/live/segment/{token}.{extension}"));
            sequence += 1;
        }
    }
    out.join("\n") + "\n"
}

// ── MPEG-TS PID normalization ───────────────────────────────────────────────

const PACKET: usize = 188;
const PMT_PID: u16 = 0x1000;
const VIDEO_TYPES: &[u8] = &[0x01, 0x02, 0x10, 0x1B, 0x24, 0x42];
const AUDIO_TYPES: &[u8] = &[0x03, 0x04, 0x0F, 0x11, 0x81, 0x87];

fn crc32_mpeg(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        crc ^= (byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04C1_1DB7 } else { crc << 1 };
        }
    }
    crc
}

fn packet_pid(packet: &[u8]) -> u16 {
    ((packet[1] as u16 & 0x1F) << 8) | packet[2] as u16
}

/// The PSI section starting in a packet with payload_unit_start set.
fn section_in(packet: &[u8]) -> Option<&[u8]> {
    if packet[1] & 0x40 == 0 {
        return None;
    }
    let control = (packet[3] >> 4) & 3;
    if control & 1 == 0 {
        return None;
    }
    let mut offset = 4;
    if control & 2 != 0 {
        offset += 1 + *packet.get(4)? as usize;
    }
    let pointer = *packet.get(offset)? as usize;
    packet.get(offset + 1 + pointer..)
}

fn section_length(section: &[u8]) -> Option<usize> {
    Some(3 + (((*section.get(1)? as usize) & 0x0F) << 8 | *section.get(2)? as usize))
}

fn section_packet(pid: u16, continuity: u8, section: &[u8]) -> [u8; PACKET] {
    let mut packet = [0xFFu8; PACKET];
    packet[..5].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10 | continuity, 0]);
    packet[5..5 + section.len()].copy_from_slice(section);
    let crc = crc32_mpeg(section).to_be_bytes();
    packet[5 + section.len()..9 + section.len()].copy_from_slice(&crc);
    packet
}

/// Rewrite a TS segment so its program always uses the same PIDs: PMT on
/// 0x1000, video on 0x100.., audio on 0x110.., anything else on 0x120...
/// Returns `None` (serve unchanged) for anything that isn't a simple
/// single-program TS segment.
fn normalize_ts(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < PACKET || data[0] != 0x47 || data.len() % PACKET != 0 {
        return None;
    }
    let packets = || data.chunks_exact(PACKET);

    let mut pmt_pid = None;
    let mut pmt: Option<&[u8]> = None;
    for packet in packets() {
        let pid = packet_pid(packet);
        let Some(section) = section_in(packet) else { continue };
        if pid == 0 && pmt_pid.is_none() && section.first() == Some(&0) {
            let len = section_length(section)?;
            let programs = section.get(8..len.checked_sub(4)?)?;
            pmt_pid = programs
                .chunks_exact(4)
                .find(|p| p[0] != 0 || p[1] != 0)
                .map(|p| ((p[2] as u16 & 0x1F) << 8) | p[3] as u16);
        } else if Some(pid) == pmt_pid && section.first() == Some(&2) {
            pmt = Some(section.get(..section_length(section)?)?);
            break;
        }
    }
    let (pmt_pid, pmt) = (pmt_pid?, pmt?);

    let pcr_pid = ((*pmt.get(8)? as u16 & 0x1F) << 8) | *pmt.get(9)? as u16;
    let info_len = ((*pmt.get(10)? as usize & 0x0F) << 8) | *pmt.get(11)? as usize;
    let program_info = pmt.get(12..12 + info_len)?;
    let mut streams = Vec::new();
    let mut at = 12 + info_len;
    while at + 5 <= pmt.len() - 4 {
        let stream_type = pmt[at];
        let pid = ((pmt[at + 1] as u16 & 0x1F) << 8) | pmt[at + 2] as u16;
        let desc_len = ((pmt[at + 3] as usize & 0x0F) << 8) | pmt[at + 4] as usize;
        streams.push((stream_type, pid, pmt.get(at + 5..at + 5 + desc_len)?));
        at += 5 + desc_len;
    }
    let kind = |t: u8| if VIDEO_TYPES.contains(&t) { 0 } else if AUDIO_TYPES.contains(&t) { 1 } else { 2 };
    streams.sort_by_key(|s| kind(s.0));

    let mut remap: HashMap<u16, u16> = HashMap::new();
    let mut counts = [0u16; 3];
    let mut entries = Vec::new();
    for (stream_type, pid, descriptors) in &streams {
        let k = kind(*stream_type);
        let new_pid = [0x100, 0x110, 0x120][k] + counts[k];
        counts[k] += 1;
        remap.insert(*pid, new_pid);
        entries.extend_from_slice(&[
            *stream_type,
            0xE0 | (new_pid >> 8) as u8,
            new_pid as u8,
            0xF0 | (descriptors.len() >> 8) as u8,
            descriptors.len() as u8,
        ]);
        entries.extend_from_slice(descriptors);
    }
    // 0x1FFF means "no PCR"; a PCR on its own PID gets a fixed one too.
    let new_pcr = if pcr_pid == 0x1FFF { pcr_pid } else { *remap.entry(pcr_pid).or_insert(0x1F0) };

    let mut pmt_body = vec![0, 1, 0xC1, 0, 0, 0xE0 | (new_pcr >> 8) as u8, new_pcr as u8];
    pmt_body.extend_from_slice(&[0xF0 | (info_len >> 8) as u8, info_len as u8]);
    pmt_body.extend_from_slice(program_info);
    pmt_body.extend_from_slice(&entries);
    let pmt_len = pmt_body.len() + 4;
    let mut pmt_section = vec![2, 0xB0 | (pmt_len >> 8) as u8, pmt_len as u8];
    pmt_section.extend_from_slice(&pmt_body);
    // Section + CRC must fit one packet after the header and pointer field.
    if pmt_section.len() + 4 > PACKET - 5 {
        return None;
    }
    // transport_stream_id 1, version 0, program 1 -> PMT_PID
    let pat_section = [0, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1, 0xE0 | (PMT_PID >> 8) as u8, PMT_PID as u8];

    let mut out = Vec::with_capacity(data.len());
    for packet in packets() {
        let pid = packet_pid(packet);
        let continuity = packet[3] & 0x0F;
        if pid == 0 || pid == pmt_pid {
            if packet[1] & 0x40 != 0 {
                let (new_pid, section) = if pid == 0 { (0, &pat_section[..]) } else { (PMT_PID, &pmt_section[..]) };
                out.extend_from_slice(&section_packet(new_pid, continuity, section));
            }
        } else if let Some(&new_pid) = remap.get(&pid) {
            out.extend_from_slice(&[0x47, (packet[1] & 0xE0) | (new_pid >> 8) as u8, new_pid as u8]);
            out.extend_from_slice(&packet[3..]);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROXY: &str = "http://127.0.0.1:1";

    fn ts_segment(pmt_pid: u16, video: u16, audio: u16) -> Vec<u8> {
        let pat = [0, 0xB0, 13, 0, 7, 0xC1, 0, 0, 0, 1, 0xE0 | (pmt_pid >> 8) as u8, pmt_pid as u8];
        let mut pmt = vec![2, 0xB0, 0, 0, 1, 0xC1, 0, 0, 0xE0 | (video >> 8) as u8, video as u8, 0xF0, 0];
        pmt.extend_from_slice(&[0x0F, 0xE0 | (audio >> 8) as u8, audio as u8, 0xF0, 0]);
        pmt.extend_from_slice(&[0x1B, 0xE0 | (video >> 8) as u8, video as u8, 0xF0, 0]);
        pmt[2] = (pmt.len() + 4 - 3) as u8;
        let es = |pid: u16, fill: u8| {
            let mut p = [fill; PACKET];
            p[..4].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x13]);
            p
        };
        let mut out = Vec::new();
        out.extend_from_slice(&section_packet(0, 0, &pat));
        out.extend_from_slice(&section_packet(pmt_pid, 0, &pmt));
        out.extend_from_slice(&es(video, 0xAA));
        out.extend_from_slice(&es(audio, 0xBB));
        out.extend_from_slice(&es(0x1FFF, 0xFF));
        out
    }

    #[test]
    fn different_packagers_normalize_to_identical_layouts() {
        let content = normalize_ts(&ts_segment(0x1000, 0x100, 0x101)).unwrap();
        let ad = normalize_ts(&ts_segment(0x100, 0x102, 0x101)).unwrap();
        // Null packet dropped; PAT, PMT, video, audio kept.
        assert_eq!(content.len(), 4 * PACKET);
        assert_eq!(content, ad);
        let pids: Vec<u16> = content.chunks_exact(PACKET).map(packet_pid).collect();
        assert_eq!(pids, vec![0, PMT_PID, 0x100, 0x110]);
        // Payloads are untouched.
        assert_eq!(content[2 * PACKET + 4], 0xAA);
        assert_eq!(content[3 * PACKET + 4], 0xBB);
        // Sections carry a valid CRC (CRC over section + CRC is zero).
        for packet in content.chunks_exact(PACKET).take(2) {
            let section = section_in(packet).unwrap();
            let len = section_length(section).unwrap();
            assert_eq!(crc32_mpeg(&section[..len]), 0);
        }
    }

    #[test]
    fn leaves_non_ts_data_alone() {
        assert!(normalize_ts(b"ID3\x04\x00 not a transport stream").is_none());
        assert!(normalize_ts(&[0x47; 100]).is_none());
    }

    #[test]
    fn master_keeps_best_variant_and_its_renditions() {
        let base = Url::parse("https://cdn.example.com/live/master.m3u8?token=1").unwrap();
        let text = "#EXTM3U\n#EXT-X-VERSION:5\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"subs\",NAME=\"English\",URI=\"subs/en.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=SUBTITLES,GROUP-ID=\"other\",NAME=\"X\",URI=\"x.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=1500000,SUBTITLES=\"subs\"\nlow/playlist.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=3000000,RESOLUTION=1280x720,SUBTITLES=\"subs\"\nhigh/playlist.m3u8\n";
        let out = rewrite_master(text, &base, PROXY).unwrap();
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "#EXTM3U");
        assert!(out.contains("URI=\"https://cdn.example.com/live/subs/en.m3u8\""));
        assert!(!out.contains("x.m3u8"));
        assert_eq!(out.matches("#EXT-X-STREAM-INF").count(), 1);
        assert!(out.contains("BANDWIDTH=3000000"));
        let media = lines.last().unwrap();
        let token = media.strip_prefix("http://127.0.0.1:1/live/media/").unwrap();
        assert_eq!(decode(token).unwrap(), "https://cdn.example.com/live/high/playlist.m3u8");
    }

    #[test]
    fn media_segments_carry_key_and_sequence_iv() {
        let base = Url::parse("https://cdn.example.com/live/720.m3u8").unwrap();
        let text = "#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXT-X-MEDIA-SEQUENCE:26\n\
            #EXT-X-KEY:METHOD=AES-128,URI=\"https://keys.example.com/k.key\"\n#EXTINF:4,\nseg26.ts\n\
            #EXT-X-DISCONTINUITY\n#EXT-X-KEY:METHOD=AES-128,URI=\"k2.key\",IV=0x0000000000000000000000000000ABCD\n#EXTINF:4,\nseg27.ts\n\
            #EXT-X-KEY:METHOD=NONE\n#EXTINF:4,\nhttps://ads.example.com/ad.ts?x=1\n";
        let out = rewrite_media(text, &base, PROXY);
        assert!(!out.contains("#EXT-X-KEY"));
        assert!(out.contains("#EXT-X-DISCONTINUITY"));
        let segments: Vec<String> = out
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(|l| decode(l.rsplit('/').next().unwrap()).unwrap())
            .collect();
        assert_eq!(
            segments,
            vec![
                format!("https://cdn.example.com/live/seg26.ts\nhttps://keys.example.com/k.key\n{:032x}", 26),
                "https://cdn.example.com/live/seg27.ts\nhttps://cdn.example.com/live/k2.key\n0000000000000000000000000000ABCD".to_string(),
                "https://ads.example.com/ad.ts?x=1\n\n".to_string(),
            ]
        );
    }

    #[test]
    fn fmp4_playlists_pass_through_with_absolute_uris() {
        let base = Url::parse("https://cdn.example.com/live/a.m3u8").unwrap();
        let text = "#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:4,\nseg1.m4s\n";
        let out = rewrite_media(text, &base, PROXY);
        assert!(out.contains("#EXT-X-MAP:URI=\"https://cdn.example.com/live/init.mp4\""));
        assert!(out.contains("\nhttps://cdn.example.com/live/seg1.m4s\n"));
    }

    #[test]
    fn decrypts_aes128_cbc() {
        use aes::cipher::{block_padding::Pkcs7, BlockEncryptMut};
        let key = [7u8; 16];
        let iv = 26u128;
        let plain = vec![0x47u8; PACKET * 2];
        let mut buf = plain.clone();
        buf.resize(plain.len() + 16, 0);
        let enc_len = cbc::Encryptor::<aes::Aes128>::new(&key.into(), &iv.to_be_bytes().into())
            .encrypt_padded_mut::<Pkcs7>(&mut buf, plain.len())
            .unwrap()
            .len();
        buf.truncate(enc_len);
        assert_eq!(decrypt(buf, &key, &format!("{iv:032x}")).unwrap(), plain);
    }
}
