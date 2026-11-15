import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, readdirSync, readFileSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, isAbsolute, join, relative, resolve, sep } from "node:path";

// Official VideoLAN binary repository. Pin both version and published digest;
// builds never resolve "latest" and the installed app never downloads codecs.
export const VLC_RUNTIME = Object.freeze({
  version: "3.0.24",
  architecture: "x64",
  url: "https://download.videolan.org/pub/videolan/vlc/3.0.24/win64/vlc-3.0.24-win64.zip",
  sha256: "fcf30850371ad10c9373cc4f0f4501e7dee49e3e9ae9f20c72fb2661a1ca6323",
  source: "https://download.videolan.org/pub/videolan/vlc/3.0.24/vlc-3.0.24.tar.xz",
});

export function verifyArchive(bytes) {
  const actual = createHash("sha256").update(bytes).digest("hex");
  if (actual !== VLC_RUNTIME.sha256) throw new Error(`VLC archive checksum mismatch: ${actual}`);
}

export function assertChild(root, path) {
  const base = realpathSync(root);
  const parent = realpathSync(dirname(path));
  const target = existsSync(path) ? realpathSync(path) : join(parent, basename(path));
  const rel = relative(base, target);
  if (!rel || isAbsolute(rel) || rel === ".." || rel.startsWith(`..${sep}`)) {
    throw new Error(`VLC runtime path must stay below ${root}: ${path}`);
  }
}

export function runtimeFiles(root) {
  const files = [];
  const walk = (directory) => {
    for (const entry of readdirSync(directory, { withFileTypes: true })) {
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) throw new Error(`Unexpected link in VLC runtime: ${path}`);
      if (entry.isDirectory()) walk(path);
      else if (entry.isFile()) files.push(relative(root, path).split(sep).join("/"));
    }
  };
  walk(root);
  return files.sort();
}

export function validateRuntime(root) {
  for (const file of ["libvlc.dll", "libvlccore.dll", "plugins/access/libhttp_plugin.dll", "plugins/demux/libadaptive_plugin.dll", "plugins/codec/libavcodec_plugin.dll", "plugins/video_output/libdirect3d11_plugin.dll", "plugins/audio_output/libmmdevice_plugin.dll"]) {
    if (!existsSync(join(root, file))) throw new Error(`Bundled VLC runtime is incomplete: ${file}`);
  }
  const manifestPath = join(root, "runtime-source.json");
  if (!existsSync(manifestPath)) throw new Error("VLC runtime provenance is missing; run npm run setup:vlc");
  const manifest = JSON.parse(readFileSync(manifestPath, "utf8"));
  if (manifest.archiveSha256 !== VLC_RUNTIME.sha256 || manifest.version !== VLC_RUNTIME.version) {
    throw new Error("VLC runtime version does not match the pinned build; run npm run setup:vlc");
  }
  // Check every DLL, including plugins, before it is copied into an installer.
  const files = runtimeFiles(root).filter((path) => path.endsWith(".dll"));
  const expected = Object.keys(manifest.files || {});
  if (files.length !== expected.length) throw new Error("VLC runtime DLL inventory has changed");
  for (const file of files) {
    const bytes = readFileSync(join(root, file));
    if (createHash("sha256").update(bytes).digest("hex") !== manifest.files[file]) {
      throw new Error(`VLC runtime DLL failed integrity check: ${file}`);
    }
    const pe = bytes.length >= 64 ? bytes.readUInt32LE(0x3c) : bytes.length;
    if (pe + 6 > bytes.length || bytes.toString("ascii", pe, pe + 4) !== "PE\0\0" || bytes.readUInt16LE(pe + 4) !== 0x8664) {
      throw new Error(`VLC runtime is not Windows x64: ${file}`);
    }
  }
  if (!runtimeFiles(root).some((file) => /copying|license/i.test(file))) throw new Error("VLC license files are missing");
  return files.length;
}

export function installRuntime(source, destination) {
  mkdirSync(destination, { recursive: true });
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    // Ship the native runtime, not the standalone VLC desktop application.
    if (entry.isFile() && (entry.name.endsWith(".dll") || /^(COPYING|AUTHORS|THANKS|NEWS|README)/i.test(entry.name))) {
      cpSync(join(source, entry.name), join(destination, entry.name));
    } else if (entry.isDirectory() && ["plugins", "lua", "locale", "hrtfs"].includes(entry.name)) {
      cpSync(join(source, entry.name), join(destination, entry.name), { recursive: true });
    }
  }
  const files = Object.fromEntries(runtimeFiles(destination).filter((path) => path.endsWith(".dll"))
    .map((path) => [path, createHash("sha256").update(readFileSync(join(destination, path))).digest("hex")]));
  writeFileSync(join(destination, "runtime-source.json"), JSON.stringify({
    version: VLC_RUNTIME.version, architecture: VLC_RUNTIME.architecture,
    archive: VLC_RUNTIME.url, archiveSha256: VLC_RUNTIME.sha256,
    source: VLC_RUNTIME.source, files,
  }, null, 2) + "\n");
  validateRuntime(destination);
}

export function copyRuntime(source, destination, allowedRoot) {
  mkdirSync(dirname(destination), { recursive: true });
  assertChild(allowedRoot, destination);
  if (realpathSync(source) === (existsSync(destination) ? realpathSync(destination) : resolve(destination))) {
    throw new Error("VLC runtime source and destination must differ");
  }
  validateRuntime(source);
  // This is a private, verified child of target/, never a system VLC folder.
  // Remove the old tree so upgrades cannot mix plugins from two VLC versions.
  if (existsSync(destination)) rmSync(destination, { recursive: true, force: true });
  mkdirSync(dirname(destination), { recursive: true });
  cpSync(source, destination, { recursive: true });
}
