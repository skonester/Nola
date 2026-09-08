#!/usr/bin/env node
// Apply fork branding to a disposable checkout, leaving upstream files unchanged in Git.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");

export function prepareNola({ root = projectRoot, version, repository, check = false } = {}) {
  const read = (path) => readFileSync(resolve(root, path), "utf8");
  const branding = JSON.parse(read("nola.json"));
  repository ??= branding.repository;
  const pkg = JSON.parse(read("package.json"));
  version ??= pkg.version;

  // Use a numeric version supported by every installer; prerelease is a release flag.
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(version)
      || version.split(".").some((part) => Number(part) > 65535)) {
    throw new Error("Version must be MAJOR.MINOR.PATCH, with each number between 0 and 65535.");
  }
  if (!/^[A-Za-z0-9-]+\/[A-Za-z0-9_.-]+$/.test(repository)
      || repository.toLowerCase() === "chwair/magnolia") {
    throw new Error("Repository must be the Nola fork in owner/repo form, not Magnolia upstream.");
  }

  // Stage every edit in memory. An upstream change that breaks an anchor fails before writing.
  const changes = new Map();
  const json = (path, edit) => {
    const value = JSON.parse(read(path));
    edit(value);
    changes.set(path, `${JSON.stringify(value, null, 2)}\n`);
  };
  const replace = (path, before, after) => {
    const source = changes.get(path) ?? read(path);
    if (source.includes(before)) {
      changes.set(path, source.replaceAll(before, after));
    } else if (!source.includes(after)) {
      throw new Error(`Nola branding anchor missing in ${path}: ${before}. Review upstream changes.`);
    }
  };

  json("src-tauri/tauri.conf.json", (config) => {
    config.productName = branding.productName;
    config.mainBinaryName = branding.mainBinaryName;
    config.identifier = branding.identifier;
    config.version = version;
    for (const window of config.app.windows) {
      if (window.title === "Magnolia" || window.title === branding.productName) {
        window.title = branding.productName;
      }
    }
  });
  json("package.json", (value) => { value.version = version; });
  json("package-lock.json", (value) => {
    value.version = version;
    value.packages[""].version = version;
  });

  const cargo = read("src-tauri/Cargo.toml");
  const packageBlock = cargo.match(/^\[package\]\r?\n[\s\S]*?(?=^\[|$(?![\s\S]))/m)?.[0];
  if (!packageBlock || !/^version = "[^"]+"/m.test(packageBlock)) {
    throw new Error("Cannot find Cargo package version. Review upstream changes.");
  }
  changes.set("src-tauri/Cargo.toml", cargo.replace(packageBlock,
    packageBlock.replace(/^version = "[^"]+"/m, `version = "${version}"`)));
  const lock = read("src-tauri/Cargo.lock");
  const cargoName = packageBlock.match(/^name = "([^"]+)"/m)?.[1];
  if (!cargoName) throw new Error("Cannot find Cargo package name. Review upstream changes.");
  const escapedName = cargoName.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const lockPattern = new RegExp(`(\\[\\[package\\]\\]\\r?\\nname = "${escapedName}"\\r?\\nversion = ")[^"]+(")`);
  if (!lockPattern.test(lock)) {
    throw new Error("Cannot find the app in Cargo.lock. Review upstream changes.");
  }
  changes.set("src-tauri/Cargo.lock", lock.replace(lockPattern, (_, before, after) => `${before}${version}${after}`));

  replace("index.html", "<title>Magnolia</title>", `<title>${branding.productName}</title>`);
  replace("src/lib/AboutModal.svelte", "<h1>Magnolia</h1>", `<h1>${branding.productName}</h1>`);
  replace("src/lib/AboutModal.svelte", "https://github.com/chwair/magnolia", `https://github.com/${repository}`);
  replace("src/lib/Onboarding.svelte", "Magnolia Logo", `${branding.productName} Logo`);
  replace("src/lib/Onboarding.svelte", "Welcome to Magnolia!", `Welcome to ${branding.productName}!`);
  replace("src/lib/Onboarding.svelte", "Magnolia requires", `${branding.productName} requires`);
  replace("src/lib/Updater.svelte", "https://api.github.com/repos/chwair/magnolia/releases/latest",
    `https://api.github.com/repos/${repository}/releases/latest`);
  replace("src/lib/Updater.svelte", "release.tag_name.replace('v', '')", "release.tag_name.replace(/^(?:nola-)?v/, '')");
  // The font HTTP handler constructs this path without a Tauri AppHandle.
  replace("src-tauri/src/torrent.rs", "com.chair.magnolia", branding.identifier);
  replace("src-tauri/src/main.rs", '"magnolia-installer.exe"', '"nola-installer.exe"');

  if (!check) {
    for (const [path, contents] of changes) writeFileSync(resolve(root, path), contents);
  }
  return { version, repository, files: [...changes.keys()] };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const result = prepareNola({
      version: process.env.NOLA_VERSION || undefined,
      repository: process.env.NOLA_REPOSITORY || undefined,
      check: process.argv.includes("--check"),
    });
    console.log(`${process.argv.includes("--check") ? "Checked" : "Prepared"} Nola ${result.version} (${result.repository})`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
