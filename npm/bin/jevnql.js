#!/usr/bin/env node
// Runs the jevnql binary, downloading it from the matching GitHub release on
// first use and caching it. No Rust, no build step.

import { spawn } from "node:child_process";
import { createWriteStream } from "node:fs";
import { chmod, mkdir, mkdtemp, readFile, rm, stat } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { pipeline } from "node:stream/promises";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const { version } = JSON.parse(await readFile(join(here, "..", "package.json"), "utf8"));

const REPO = "Adityakhalkar/JevNQL";
const TARGETS = {
  "darwin arm64": "aarch64-apple-darwin",
  "darwin x64": "x86_64-apple-darwin",
  "linux x64": "x86_64-unknown-linux-gnu",
  "linux arm64": "aarch64-unknown-linux-gnu",
};

const target = TARGETS[`${process.platform} ${process.arch}`];
if (!target) {
  console.error(
    `jevnql has no prebuilt binary for ${process.platform} ${process.arch}.\n` +
      `Build it instead:  cargo install --git https://github.com/${REPO} jevnql-cli`,
  );
  process.exit(1);
}

const home = process.env.HOME || process.env.USERPROFILE || tmpdir();
const cache = join(process.env.JEVNQL_CACHE_DIR || join(home, ".cache", "jevnql"), `${version}-${target}`);
const binary = join(cache, "jevnql");

async function exists(path) {
  return stat(path).then(
    () => true,
    () => false,
  );
}

async function download() {
  const url = `https://github.com/${REPO}/releases/download/v${version}/jevnql-${target}.tar.gz`;
  process.stderr.write(`Downloading jevnql v${version} (${target})…\n`);
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    throw new Error(`${url} returned ${response.status} ${response.statusText}`);
  }
  const staging = await mkdtemp(join(tmpdir(), "jevnql-"));
  const archive = join(staging, "jevnql.tar.gz");
  try {
    await pipeline(response.body, createWriteStream(archive));
    await mkdir(cache, { recursive: true });
    await new Promise((resolve, reject) => {
      const tar = spawn("tar", ["-xzf", archive, "-C", cache], { stdio: ["ignore", "ignore", "inherit"] });
      tar.on("error", reject);
      tar.on("exit", (code) => (code === 0 ? resolve() : reject(new Error(`tar exited with ${code}`))));
    });
    await chmod(binary, 0o755);
  } finally {
    await rm(staging, { recursive: true, force: true });
  }
}

if (!(await exists(binary))) {
  try {
    await download();
  } catch (error) {
    console.error(`Could not download jevnql: ${error.message}`);
    process.exit(1);
  }
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
child.on("error", (error) => {
  console.error(`Could not run ${binary}: ${error.message}`);
  process.exit(1);
});
child.on("exit", (code, signal) => process.exit(signal ? 1 : (code ?? 0)));
