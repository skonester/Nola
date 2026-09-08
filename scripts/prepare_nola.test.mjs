import assert from "node:assert/strict";
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, resolve, sep } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { prepareNola } from "./prepare_nola.mjs";

const projectRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const fixtureParent = resolve(projectRoot, "build", "nola-tests");
const files = [
  "nola.json", "package.json", "package-lock.json", "src-tauri/Cargo.toml",
  "src-tauri/Cargo.lock", "src-tauri/tauri.conf.json", "src-tauri/tauri.macos.conf.json",
  "src-tauri/tauri.runtime.macos.json", "src-tauri/tauri.runtime.windows.json",
  "index.html", "src/lib/AboutModal.svelte", "src/lib/Onboarding.svelte",
  "src/lib/Updater.svelte", "src-tauri/src/torrent.rs", "src-tauri/src/main.rs",
];

function fixture(t) {
  mkdirSync(fixtureParent, { recursive: true });
  const root = mkdtempSync(resolve(fixtureParent, "checkout-"));
  t.after(() => {
    assert.ok(resolve(root).startsWith(`${fixtureParent}${sep}`));
    rmSync(root, { recursive: true, force: true });
  });
  for (const file of files) {
    const target = resolve(root, file);
    mkdirSync(dirname(target), { recursive: true });
    copyFileSync(resolve(projectRoot, file), target);
  }
  return root;
}
const read = (root, file) => readFileSync(resolve(root, file), "utf8");
const snapshot = (root) => files.map((file) => read(root, file));

test("brands a release, isolates updates/data, and preserves artwork and runtime configuration", (t) => {
  const root = fixture(t);
  const original = JSON.parse(read(root, "src-tauri/tauri.conf.json"));
  const result = prepareNola({ root, version: "3.2.7", repository: "example/Nola" });
  const config = JSON.parse(read(root, "src-tauri/tauri.conf.json"));
  assert.equal(config.productName, "Nola");
  assert.equal(config.mainBinaryName, "nola");
  assert.equal(config.identifier, "com.skonester.nola");
  assert.equal(config.app.windows[0].title, "Nola");
  assert.deepEqual(config.bundle, original.bundle);
  for (const file of files.filter((file) => /tauri\.(macos\.conf|runtime\.)/.test(file))) {
    assert.equal(read(root, file), read(projectRoot, file));
  }
  for (const file of ["package.json", "package-lock.json", "src-tauri/tauri.conf.json"]) {
    assert.equal(JSON.parse(read(root, file)).version, "3.2.7");
  }
  assert.equal(JSON.parse(read(root, "package-lock.json")).packages[""].version, "3.2.7");
  assert.match(read(root, "src-tauri/Cargo.toml"), /\[package\]\r?\nname = "magnolia-tauri-app"\r?\nversion = "3.2.7"/);
  assert.match(read(root, "src-tauri/Cargo.lock"), /name = "magnolia-tauri-app"\r?\nversion = "3.2.7"/);
  assert.match(read(root, "src/lib/Onboarding.svelte"), /Welcome to Nola!/);
  assert.match(read(root, "src/lib/Onboarding.svelte"), /media\/magnolia.png/);
  assert.match(read(root, "src/lib/AboutModal.svelte"), /https:\/\/github.com\/example\/Nola/);
  assert.match(read(root, "src/lib/Updater.svelte"), /https:\/\/api.github.com\/repos\/example\/Nola\/releases\/latest/);
  assert.ok(!read(root, "src/lib/Updater.svelte").includes("repos/chwair/magnolia"));
  assert.ok(!read(root, "src-tauri/src/torrent.rs").includes("com.chair.magnolia"));
  const updater = read(root, "src/lib/Updater.svelte");
  const expression = updater.match(/latestVersion = (release\.tag_name\.replace\([^;]+\));/)[1];
  const parseTag = new Function("release", `return ${expression}`);
  assert.equal(parseTag({ tag_name: "nola-v3.2.7" }), "3.2.7");
  assert.equal(parseTag({ tag_name: "v3.2.7" }), "3.2.7");
  assert.ok(result.files.length > 0);
  const first = snapshot(root);
  prepareNola({ root, version: "3.2.7", repository: "example/Nola" });
  assert.deepEqual(snapshot(root), first, "repeating preparation must be idempotent");
});

test("check mode and invalid inputs never modify the checkout", (t) => {
  const root = fixture(t);
  const before = snapshot(root);
  prepareNola({ root, version: "3.2.7", check: true });
  for (const version of ["v1.2.3", "1.2", "01.2.3", "1.2.3-beta", "65536.0.0", "1.2.3'; exit 0"]) {
    assert.throws(() => prepareNola({ root, version }), /Version must/);
  }
  for (const repository of ["chwair/magnolia", "CHWAIR/Magnolia", "bad/repo/extra", "owner/repo'bad"]) {
    assert.throws(() => prepareNola({ root, repository }), /Repository must/);
  }
  assert.deepEqual(snapshot(root), before);
});

test("upstream drift fails before any partial branding or version writes", (t) => {
  const root = fixture(t);
  const path = resolve(root, "src/lib/Updater.svelte");
  writeFileSync(path, read(root, "src/lib/Updater.svelte").replace("releases/latest", "releases?per_page=1"));
  const before = snapshot(root);
  assert.throws(() => prepareNola({ root, version: "3.2.7" }), /anchor missing.*Updater.svelte/);
  assert.deepEqual(snapshot(root), before);
});
