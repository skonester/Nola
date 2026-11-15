import assert from "node:assert/strict";
import test from "node:test";
import { channelKey, guideKey, migrateChannelList, indexGuide, nowAndNext, programmeProgress } from "../src/lib/liveTvGuide.js";
import { unpackVlcFrame } from "../src/lib/vlcFrames.js";

test("VLC binary frames reach the canvas without JSON or color conversion", () => {
  const packet = new Uint8Array(24);
  packet.set([78, 76, 86, 49]);
  const header = new DataView(packet.buffer);
  header.setUint32(4, 2, true); header.setUint32(8, 1, true); header.setUint32(12, 9, true);
  packet.set([241, 31, 11, 255, 10, 220, 30, 255], 16);
  const frame = unpackVlcFrame(packet.buffer);
  assert.deepEqual([frame.width, frame.height, frame.serial], [2, 1, 9]);
  assert.deepEqual([...frame.pixels], [241, 31, 11, 255, 10, 220, 30, 255]);
  assert.equal(frame.pixels.buffer, packet.buffer);
  assert.equal(unpackVlcFrame(new ArrayBuffer(0)), null);
  assert.throws(() => unpackVlcFrame(packet.slice(0, 20)), /dimensions/);
});

test("favorites survive rotated Plex tokens and migrate to source/channel identities", () => {
  const channel = { source_id: "plex-us", tvg_id: "news", url: "https://epg.provider.plex.tv/library/parts/news/?X-Plex-Token=new" };
  const old = "https://epg.provider.plex.tv/library/parts/news/?X-Plex-Token=old";
  assert.deepEqual(migrateChannelList([old, channel.url, 42], [channel]), [channelKey(channel)]);
  assert.deepEqual(migrateChannelList([channelKey(channel), "missing-channel"], [channel]), [channelKey(channel), "missing-channel"]);
  assert.notEqual(channelKey(channel), channelKey({ ...channel, source_id: "plex-ca" }));
});

test("programmes are scoped by source and sorted for now, next, and schedule gaps", () => {
  const a = { source_id: "a", tvg_id: "news" };
  const b = { source_id: "b", tvg_id: "news" };
  const current = { channel_id: "news", start: 100, stop: 200, title: "Current" };
  const next = { channel_id: "news", start: 300, stop: 400, title: "Next" };
  const index = indexGuide({ sources: [
    { source_id: "a", programmes: [next, current] },
    { source_id: "b", programmes: [{ ...current, title: "Different provider" }] },
  ] });
  assert.deepEqual(nowAndNext(index.get(guideKey(a)), 150), { current, next });
  assert.equal(nowAndNext(index.get(guideKey(b)), 150).current.title, "Different provider");
  assert.deepEqual(nowAndNext(index.get(guideKey(a)), 250), { current: undefined, next });
  assert.equal(nowAndNext(index.get(guideKey(a)), 200).current, undefined);
  assert.deepEqual(nowAndNext(undefined, 150), { current: undefined, next: undefined });
  assert.equal(programmeProgress(current, 150), 50);
  assert.equal(programmeProgress(current, 50), 0);
  assert.equal(programmeProgress(current, 250), 100);
});
