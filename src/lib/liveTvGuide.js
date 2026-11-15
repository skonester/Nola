// Stable channel identities keep favorites and history through refreshed URLs.
export const channelKey = (channel) => JSON.stringify([
  channel.source_id, channel.tvg_id || channel.url,
]);

function legacyStreamKey(value) {
  try {
    const url = new URL(value);
    // Upstream obtains a new anonymous Plex token when it generates playlists.
    if (url.hostname === "epg.provider.plex.tv") {
      for (const key of [...url.searchParams.keys()]) {
        if (key.toLowerCase() === "x-plex-token") url.searchParams.delete(key);
      }
    }
    return url.toString();
  } catch {
    return value;
  }
}

export function migrateChannelList(entries, channels) {
  const byUrl = new Map(channels.map((channel) => [legacyStreamKey(channel.url), channelKey(channel)]));
  return [...new Set(entries.filter((entry) => typeof entry === "string")
    .map((entry) => byUrl.get(legacyStreamKey(entry)) || entry))];
}

export const guideKey = (channel) => JSON.stringify([channel.source_id, channel.tvg_id]);

export function indexGuide(guide) {
  const index = new Map();
  for (const source of guide.sources || []) {
    for (const programme of source.programmes || []) {
      const key = JSON.stringify([source.source_id, programme.channel_id]);
      if (!index.has(key)) index.set(key, []);
      index.get(key).push(programme);
    }
  }
  for (const programmes of index.values()) programmes.sort((a, b) => a.start - b.start);
  return index;
}

export function nowAndNext(programmes = [], now = Date.now() / 1000) {
  const current = programmes.find((p) => p.start <= now && p.stop > now);
  const next = programmes.find((p) => p.start >= (current?.stop ?? now) && p.start > now);
  return { current, next };
}

export const programmeTime = (timestamp) => new Date(timestamp * 1000)
  .toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });

export function programmeProgress(programme, now) {
  return Math.max(0, Math.min(100, (now - programme.start) / (programme.stop - programme.start) * 100));
}
