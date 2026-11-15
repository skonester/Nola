export function unpackVlcFrame(payload) {
  const bytes = payload instanceof ArrayBuffer ? new Uint8Array(payload)
    : ArrayBuffer.isView(payload) ? new Uint8Array(payload.buffer, payload.byteOffset, payload.byteLength)
    : Uint8Array.from(payload || []);
  if (!bytes.length) return null;
  if (bytes.length < 16 || bytes[0] !== 78 || bytes[1] !== 76 || bytes[2] !== 86 || bytes[3] !== 49) {
    throw new Error("Invalid video frame received from the player.");
  }
  const header = new DataView(bytes.buffer, bytes.byteOffset, 16);
  const width = header.getUint32(4, true), height = header.getUint32(8, true);
  if (!width || !height || width * height * 4 !== bytes.length - 16) {
    throw new Error("Invalid video frame dimensions.");
  }
  return { width, height, serial: header.getUint32(12, true),
    pixels: new Uint8ClampedArray(bytes.buffer, bytes.byteOffset + 16, bytes.length - 16) };
}
