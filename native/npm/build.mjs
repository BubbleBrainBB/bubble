#!/usr/bin/env node
// Builds Bubble's npm packages into native/npm/dist:
//   node native/npm/build.mjs --version 1.0.0 [--target <rust triple>]... [--no-build] [--pack]
// Without --target it builds this machine's platform. The main package always
// lists every platform package, built here or not (CI builds the others).
import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const native = join(here, "..");
const dist = join(here, "dist");

const PLATFORMS = {
  "aarch64-apple-darwin": { name: "darwin-arm64", os: "darwin", cpu: "arm64" },
  "x86_64-apple-darwin": { name: "darwin-x64", os: "darwin", cpu: "x64" },
  "x86_64-unknown-linux-gnu": { name: "linux-x64", os: "linux", cpu: "x64" },
  "aarch64-unknown-linux-gnu": { name: "linux-arm64", os: "linux", cpu: "arm64" },
  "x86_64-pc-windows-msvc": { name: "win32-x64", os: "win32", cpu: "x64" },
};

function hostTriple() {
  const key = `${process.platform}-${process.arch}`;
  const found = Object.entries(PLATFORMS).find(([, p]) => p.name === key);
  if (!found) throw new Error(`no npm platform for this machine (${key})`);
  return found[0];
}

const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const values = (name) => args.flatMap((a, i) => (a === name ? [args[i + 1]] : []));
const version = values("--version")[0];
if (!version || !/^\d+\.\d+\.\d+(-[\w.]+)?$/.test(version)) {
  console.error("usage: node native/npm/build.mjs --version <x.y.z> [--target <triple>]... [--no-build] [--pack] [--platforms-only]");
  process.exit(2);
}
const targets = values("--target").length > 0 ? values("--target") : [hostTriple()];
const pkgName = (p) => `@bubblebrain-ai/bubble-${p.name}`;

rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });

for (const triple of targets) {
  const platform = PLATFORMS[triple];
  if (!platform) throw new Error(`unknown target ${triple}`);
  const host = triple === hostTriple();
  if (!flag("--no-build")) {
    const cargo = ["build", "--release", "-p", "bubble-cli", ...(host ? [] : ["--target", triple])];
    console.log(`cargo ${cargo.join(" ")}`);
    execFileSync("cargo", cargo, { cwd: native, stdio: "inherit", env: { ...process.env, BUBBLE_VERSION: version } });
  }
  const exe = platform.os === "win32" ? "bubble.exe" : "bubble";
  const built = join(native, "target", ...(host ? [] : [triple]), "release", exe);
  if (!existsSync(built)) throw new Error(`no binary at ${built}`);
  const dir = join(dist, platform.name);
  mkdirSync(join(dir, "bin"), { recursive: true });
  copyFileSync(built, join(dir, "bin", exe));
  chmodSync(join(dir, "bin", exe), 0o755);
  writeFileSync(
    join(dir, "package.json"),
    JSON.stringify(
      {
        name: pkgName(platform),
        version,
        description: `Bubble for ${platform.os} ${platform.cpu}`,
        repository: { type: "git", url: "git+https://github.com/BubbleBrainBB/bubble.git" },
        os: [platform.os],
        cpu: [platform.cpu],
        files: ["bin"],
        preferUnplugged: true,
      },
      null,
      2,
    ) + "\n",
  );
  writeFileSync(join(dir, "README.md"), `The ${platform.name} build of [@bubblebrain-ai/bubble](https://www.npmjs.com/package/@bubblebrain-ai/bubble).\n`);
  console.log(`packaged ${pkgName(platform)}@${version}`);
}

if (!flag("--platforms-only")) {
  const main = JSON.parse(readFileSync(join(here, "bubble", "package.json"), "utf8"));
  main.version = version;
  main.optionalDependencies = Object.fromEntries(Object.values(PLATFORMS).map((p) => [pkgName(p), version]));
  const dir = join(dist, "bubble");
  cpSync(join(here, "bubble"), dir, { recursive: true });
  writeFileSync(join(dir, "package.json"), JSON.stringify(main, null, 2) + "\n");
  console.log(`packaged @bubblebrain-ai/bubble@${version}`);
}

if (flag("--pack")) {
  const tarballs = join(dist, "tarballs");
  mkdirSync(tarballs, { recursive: true });
  for (const name of [...targets.map((t) => PLATFORMS[t].name), ...(flag("--platforms-only") ? [] : ["bubble"])]) {
    execFileSync("npm", ["pack", "--pack-destination", tarballs], { cwd: join(dist, name), stdio: "inherit" });
  }
}
