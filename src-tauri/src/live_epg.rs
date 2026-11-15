//! XMLTV parsing and bounded gzip decoding. Times are stored as UTC seconds;
//! the UI formats them in the viewer's local timezone.

use chrono::{DateTime, NaiveDateTime};
use flate2::read::MultiGzDecoder;
use quick_xml::{events::Event, Reader};
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, io::Read};

pub const MAX_XML_BYTES: usize = 128 * 1024 * 1024;
const MAX_PROGRAMMES: usize = 500_000;
const GUIDE_DAYS: i64 = 7;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Programme {
    pub channel_id: String,
    pub start: i64,
    pub stop: i64,
    pub title: String,
    pub subtitle: String,
    pub description: String,
}

pub fn decode(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return if bytes.len() <= MAX_XML_BYTES {
            Ok(bytes.to_vec())
        } else {
            Err("Program guide exceeds 128 MB".into())
        };
    }
    let mut decoded = Vec::new();
    MultiGzDecoder::new(bytes)
        .take(MAX_XML_BYTES as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(|_| "Could not decompress program guide".to_string())?;
    if decoded.len() > MAX_XML_BYTES {
        return Err("Program guide exceeds 128 MB".into());
    }
    Ok(decoded)
}

fn timestamp(value: &str) -> Option<i64> {
    let value = value.trim();
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    let format = match digits {
        14 => "%Y%m%d%H%M%S",
        12 => "%Y%m%d%H%M",
        _ => return None,
    };
    let offset = value[digits..].trim();
    if offset.is_empty() || offset == "Z" || offset == "UTC" {
        NaiveDateTime::parse_from_str(&value[..digits], format)
            .ok()
            .map(|t| t.and_utc().timestamp())
    } else {
        DateTime::parse_from_str(
            &format!("{} {}", &value[..digits], offset),
            &format!("{format} %z"),
        )
        .ok()
        .map(|t| t.timestamp())
    }
}

/// Keep only channels present in this source and a rolling seven-day window.
/// XMLTV permits an omitted stop time; infer it from the next programme.
pub fn parse(
    bytes: &[u8],
    channel_ids: &HashSet<String>,
    now: i64,
) -> Result<Vec<Programme>, String> {
    let text =
        std::str::from_utf8(bytes).map_err(|_| "Program guide is not UTF-8 XMLTV".to_string())?;
    let mut reader = Reader::from_str(text.trim_start_matches('\u{feff}'));
    let mut programmes = Vec::new();
    let mut current: Option<Programme> = None;
    let mut field = String::new();
    let mut value = String::new();
    let mut saw_tv = false;
    let mut closed_tv = false;
    let mut depth = 0usize;
    let mut count = 0usize;
    loop {
        match reader
            .read_event()
            .map_err(|e| format!("Invalid XMLTV guide: {e}"))?
        {
            Event::Start(tag) => {
                depth += 1;
                match tag.name().as_ref() {
                    b"tv" if depth == 1 => saw_tv = true,
                    b"programme" if saw_tv && depth == 2 => {
                        count += 1;
                        if count > MAX_PROGRAMMES {
                            return Err("Program guide has too many programmes".into());
                        }
                        let mut channel_id = String::new();
                        let mut start = None;
                        let mut stop = None;
                        for attr in tag.attributes() {
                            let attr = attr.map_err(|e| e.to_string())?;
                            let value = attr.unescape_value().map_err(|e| e.to_string())?;
                            match attr.key.as_ref() {
                                b"channel" => channel_id = value.into_owned(),
                                b"start" => start = timestamp(&value),
                                b"stop" => stop = timestamp(&value),
                                _ => {}
                            }
                        }
                        current = start
                            .filter(|t| *t < now + GUIDE_DAYS * 86400)
                            .filter(|_| channel_ids.contains(&channel_id))
                            .map(|start| Programme {
                                channel_id,
                                start,
                                stop: stop.unwrap_or(0),
                                title: String::new(),
                                subtitle: String::new(),
                                description: String::new(),
                            });
                    }
                    b"title" | b"sub-title" | b"desc" if current.is_some() && depth == 3 => {
                        field = String::from_utf8_lossy(tag.name().as_ref()).into_owned();
                        value.clear();
                    }
                    _ => {}
                }
            }
            Event::Text(text) if !field.is_empty() => {
                value.push_str(&text.unescape().map_err(|e| e.to_string())?);
            }
            Event::CData(text) if !field.is_empty() => {
                value.push_str(&text.decode().map_err(|e| e.to_string())?);
            }
            Event::Empty(tag) if tag.name().as_ref() == b"tv" && depth == 0 => {
                saw_tv = true;
                closed_tv = true;
            }
            Event::End(tag) => {
                if let Some(programme) = current.as_mut() {
                    if depth == 3 && tag.name().as_ref() == field.as_bytes() {
                        let target = match field.as_str() {
                            "title" => &mut programme.title,
                            "sub-title" => &mut programme.subtitle,
                            _ => &mut programme.description,
                        };
                        // XMLTV can contain translations; use the first nonempty one.
                        if target.is_empty() {
                            *target = value.trim().to_string();
                        }
                        field.clear();
                    }
                }
                if tag.name().as_ref() == b"programme" && depth == 2 {
                    if let Some(programme) = current.take().filter(|p| !p.title.is_empty()) {
                        programmes.push(programme);
                    }
                    field.clear();
                }
                if tag.name().as_ref() == b"tv" && depth == 1 {
                    closed_tv = true;
                }
                depth = depth.saturating_sub(1);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !saw_tv || !closed_tv || depth != 0 {
        return Err("This is not a complete XMLTV program guide".into());
    }
    programmes.sort_by(|a, b| (&a.channel_id, a.start).cmp(&(&b.channel_id, b.start)));
    for index in 0..programmes.len().saturating_sub(1) {
        if programmes[index].stop == 0
            && programmes[index].channel_id == programmes[index + 1].channel_id
        {
            programmes[index].stop = programmes[index + 1].start;
        }
    }
    programmes.retain(|p| p.stop > p.start && p.stop > now - 3600);
    programmes
        .dedup_by(|a, b| a.channel_id == b.channel_id && a.start == b.start && a.stop == b.stop);
    Ok(programmes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NOW: i64 = 1794744000; // 2026-11-15 12:00 UTC

    #[test]
    fn reads_offsets_entities_cdata_and_translations() {
        let ids = HashSet::from(["news&tv".into()]);
        let xml = br#"<?xml version="1.0"?><!DOCTYPE tv SYSTEM "xmltv.dtd"><tv>
          <programme channel="news&amp;tv" start="20261115060000 -0600" stop="20261115070000 -0600">
            <title>News &amp; Weather</title><title lang="fr">Autre</title>
            <sub-title><![CDATA[Today <live>]]></sub-title><desc>Morning news &#38; more.</desc>
          </programme>
          <programme channel="other" start="20261115120000 +0000" stop="20261115130000 +0000"><title>Skip</title></programme>
        </tv>"#;
        let programmes = parse(xml, &ids, NOW).unwrap();
        assert_eq!(programmes.len(), 1);
        assert_eq!(programmes[0].start, NOW);
        assert_eq!(programmes[0].title, "News & Weather");
        assert_eq!(programmes[0].subtitle, "Today <live>");
        assert_eq!(programmes[0].description, "Morning news & more.");
    }

    #[test]
    fn infers_missing_stop_sorts_and_filters_expired_or_invalid_programmes() {
        let xml = br#"<tv>
          <programme channel="a" start="20261115130000" stop="20261115140000"><title>Next</title></programme>
          <programme channel="a" start="20261115120000"><title>Now</title></programme>
          <programme channel="a" start="20251115120000" stop="20251115130000"><title>Old</title></programme>
          <programme channel="a" start="invalid" stop="20261115130000"><title>Bad</title></programme>
        </tv>"#;
        let programmes = parse(xml, &HashSet::from(["a".into()]), NOW).unwrap();
        assert_eq!(programmes.len(), 2);
        assert_eq!(programmes[0].title, "Now");
        assert_eq!(programmes[0].stop, programmes[1].start);
    }

    #[test]
    fn rejects_html_and_truncated_xml_but_accepts_empty_guides() {
        let ids = HashSet::new();
        assert!(parse(b"<html>unavailable</html>", &ids, NOW).is_err());
        assert!(parse(b"<tv><programme", &ids, NOW).is_err());
        assert!(parse(b"<tv>", &ids, NOW).is_err());
        assert!(parse(b"<tv/>", &ids, NOW).unwrap().is_empty());
    }

    #[test]
    fn detects_gzip_by_content_instead_of_filename() {
        let xml = b"<tv/>";
        let mut writer = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        writer.write_all(xml).unwrap();
        assert_eq!(decode(&writer.finish().unwrap()).unwrap(), xml);
        assert_eq!(decode(xml).unwrap(), xml);
        assert!(decode(&[0x1f, 0x8b, 0]).is_err());
    }
}
