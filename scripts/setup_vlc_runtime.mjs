#!/usr/bin/env node
import { existsSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { VLC_RUNTIME, verifyArchive, validateRuntime, installRuntime, assertChild } from "./vlc_runtime.mjs";

const projectRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const runtimeRoot = join(projectRoot, "src-tauri", "libs", "vlc");
const targetRoot = join(projectRoot, "build");
const args = process.argv.slice(2);

async function main() {
  if (args.includes("--check")) {
    console.log(`[VLC] Verified ${validateRuntime(runtimeRoot)} bundled x64 DLLs (${VLC_RUNTIME.version}).`);
    return;
  }
  if (process.platform !== "win32" || !["x64", "x86_64"].includes(process.env.VLC_TARGET_ARCH || process.arch)) {
    throw new Error("The bundled VLC runtime currently targets Windows x64.");
  }
  if (existsSync(runtimeRoot)) {
    try { validateRuntime(runtimeRoot); console.log(`[VLC] Runtime ${VLC_RUNTIME.version} is already installed.`); return; } catch {}
  }
  mkdirSync(targetRoot, { recursive: true });
  const staging = mkdtempSync(join(targetRoot, "vlc-setup-"));
  const removeStaging = () => {
    assertChild(targetRoot, staging);
    rmSync(staging, { recursive: true, force: true });
  };
  try {
    const archive = join(targetRoot, `vlc-${VLC_RUNTIME.version}-win64.zip`);
    if (process.env.VLC_ARCHIVE_PATH) {
      writeFileSync(archive, readFileSync(resolve(process.env.VLC_ARCHIVE_PATH)));
    } else if (!existsSync(archive)) {
      console.log(`[VLC] Downloading official VideoLAN ${VLC_RUNTIME.version} runtime…`);
      const response = await fetch(VLC_RUNTIME.url, { signal: AbortSignal.timeout(180_000) });
      if (!response.ok) throw new Error(`VLC runtime download failed: HTTP ${response.status}`);
      writeFileSync(archive, Buffer.from(await response.arrayBuffer()));
    }
    verifyArchive(readFileSync(archive));
    const extract = join(staging, "unpacked");
    mkdirSync(extract);
    const escapedArchive = archive.replace(/'/g, "''");
    const escapedExtract = extract.replace(/'/g, "''");
    const result = spawnSync("powershell.exe", ["-NoProfile", "-NonInteractive", "-Command",
      `Expand-Archive -LiteralPath '${escapedArchive}' -DestinationPath '${escapedExtract}'`], { stdio: "inherit", windowsHide: true });
    if (result.status !== 0) throw new Error("Could not extract the official VLC runtime archive");
    const prepared = join(staging, "runtime");
    installRuntime(join(extract, `vlc-${VLC_RUNTIME.version}`), prepared);
    // Keep the previous runtime intact until the complete replacement validates.
    mkdirSync(dirname(runtimeRoot), { recursive: true });
    assertChild(projectRoot, runtimeRoot);
    assertChild(projectRoot, prepared);
    if (existsSync(runtimeRoot)) renameSync(runtimeRoot, join(staging, "previous-runtime"));
    try { renameSync(prepared, runtimeRoot); }
    catch (error) {
      if (existsSync(join(staging, "previous-runtime"))) renameSync(join(staging, "previous-runtime"), runtimeRoot);
      throw error;
    }
    console.log(`[VLC] Installed ${validateRuntime(runtimeRoot)} DLLs and plugins in ${runtimeRoot}.`);
  } finally { removeStaging(); }
}

main().catch((error) => { console.error(`[VLC] ${error.message}`); process.exitCode = 1; });
