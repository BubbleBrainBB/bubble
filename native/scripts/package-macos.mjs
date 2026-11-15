#!/usr/bin/env node
// Packages Bubble.app for macOS with everything it needs inside it: the NSH
// app, Bubble's agent (the TypeScript core the app starts as
// `bubble host --stdio`) and the Node.js that runs it.
//
//   node native/scripts/package-macos.mjs [--bubble <npm version>] [--node-version <x.y.z>]
//                                         [--out <dir>] [--no-app]
//
// Without --bubble the agent is this checkout's own build, packed the way
// `npm publish` packs it. With --bubble it is that published version.
// --no-app keeps the .app already in the output directory and only replaces
// the agent and Node inside it.
//
// The app looks for `Contents/Resources/bubble/dist/main.js` and
// `Contents/Resources/node/bin/node` (crates/bubble-host/src/launch.rs).
import { execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const native = join(here, "..");
const repo = join(native, "..");

// The Node the app ships. Bubble needs 22.19 or newer.
const NODE_VERSION = "24.18.1";

const args = process.argv.slice(2);
const flag = (name) => args.includes(name);
const value = (name) => {
  const at = args.indexOf(name);
  return at >= 0 ? args[at + 1] : undefined;
};
if (flag("--help") || flag("-h")) {
  console.log(readFileSync(fileURLToPath(import.meta.url), "utf8").split("\n").slice(1, 15).map((l) => l.replace(/^\/\/ ?/, "")).join("\n"));
  process.exit(0);
}
if (process.platform !== "darwin") {
  console.error("package-macos.mjs runs on macOS");
  process.exit(2);
}

const bubbleVersion = value("--bubble");
const nodeVersion = value("--node-version") ?? NODE_VERSION;
const out = value("--out") ?? join(native, "target", "package");
const manifest = JSON.parse(readFileSync(join(native, "app", "nsh.json"), "utf8"));
const app = join(out, `${manifest.name}.app`);
const resources = join(app, "Contents", "Resources");

function run(command, commandArgs, options = {}) {
  console.log(`$ ${command} ${commandArgs.join(" ")}`);
  execFileSync(command, commandArgs, { stdio: "inherit", ...options });
}

function step(title) {
  console.log(`\n== ${title}`);
}

// 1. The app itself.
if (flag("--no-app")) {
  if (!existsSync(app)) {
    console.error(`--no-app: ${app} is not there yet`);
    process.exit(1);
  }
} else {
  step("Packaging the app");
  run("nsh", ["package", "--target", "macos", "-o", out], { cwd: join(native, "app") });
}

// 2. Node.js, from nodejs.org, checked against its published checksum and
// kept under target/ so the next run does not download it again.
step(`Node.js ${nodeVersion}`);
const arch = process.arch === "arm64" ? "arm64" : "x64";
const nodeName = `node-v${nodeVersion}-darwin-${arch}`;
const nodeCache = join(native, "target", "node");
const nodeHome = join(nodeCache, nodeName);
if (!existsSync(join(nodeHome, "bin", "node"))) {
  mkdirSync(nodeCache, { recursive: true });
  const base = `https://nodejs.org/dist/v${nodeVersion}`;
  const tarball = join(nodeCache, `${nodeName}.tar.gz`);
  const sums = join(nodeCache, `SHASUMS256-v${nodeVersion}.txt`);
  run("curl", ["-fL", "--retry", "3", "-o", tarball, `${base}/${nodeName}.tar.gz`]);
  run("curl", ["-fL", "--retry", "3", "-o", sums, `${base}/SHASUMS256.txt`]);
  const expected = readFileSync(sums, "utf8")
    .split("\n")
    .map((line) => line.trim().split(/\s+/))
    .find(([, file]) => file === `${nodeName}.tar.gz`)?.[0];
  const actual = createHash("sha256").update(readFileSync(tarball)).digest("hex");
  if (!expected || expected !== actual) {
    rmSync(tarball, { force: true });
    console.error(`checksum of ${nodeName}.tar.gz is ${actual}, nodejs.org lists ${expected ?? "none"}`);
    process.exit(1);
  }
  run("tar", ["-xzf", tarball, "-C", nodeCache]);
  rmSync(tarball, { force: true });
} else {
  console.log(`using ${nodeHome}`);
}
const node = join(nodeHome, "bin", "node");
const npmCli = join(nodeHome, "lib", "node_modules", "npm", "bin", "npm-cli.js");

// 3. Bubble's agent, installed by the Node that will run it, with its
// dependencies inside its own folder (the layout of a global install).
step(bubbleVersion ? `Bubble ${bubbleVersion} from npm` : "Bubble from this checkout");
const stage = mkdtempSync(join(tmpdir(), "bubble-package-"));
try {
  let spec = `@bubblebrain-ai/bubble@${bubbleVersion}`;
  if (!bubbleVersion) {
    run("npm", ["pack", "--pack-destination", stage], { cwd: repo });
    const tarballs = readdirSync(stage).filter((file) => file.endsWith(".tgz"));
    if (tarballs.length !== 1) throw new Error(`expected one packed tarball, found ${tarballs.length}`);
    spec = join(stage, tarballs[0]);
  }
  writeFileSync(join(stage, "package.json"), JSON.stringify({ name: "bubble-app-agent", private: true }));
  run(node, [npmCli, "install", "--install-strategy=shallow", "--omit=dev", "--no-audit", "--no-fund", spec], {
    cwd: stage,
    env: { ...process.env, PATH: `${join(nodeHome, "bin")}:${process.env.PATH ?? ""}` },
  });
  const installed = join(stage, "node_modules", "@bubblebrain-ai", "bubble");
  const stray = readdirSync(join(stage, "node_modules")).filter((name) => name !== "@bubblebrain-ai" && !name.startsWith("."));
  if (stray.length > 0) throw new Error(`dependencies were installed outside the package: ${stray.join(", ")}`);

  rmSync(join(resources, "bubble"), { recursive: true, force: true });
  rmSync(join(resources, "node"), { recursive: true, force: true });
  cpSync(installed, join(resources, "bubble"), { recursive: true, verbatimSymlinks: true });
  mkdirSync(join(resources, "node", "bin"), { recursive: true });
  cpSync(node, join(resources, "node", "bin", "node"));
  cpSync(join(nodeHome, "LICENSE"), join(resources, "node", "LICENSE"));
} finally {
  rmSync(stage, { recursive: true, force: true });
}

// 4. What was added changes the app's seal, so it is signed again (ad hoc,
// as `nsh package` signs it).
step("Signing");
run("codesign", ["--force", "--sign", "-", app]);
run("codesign", ["--verify", "--strict", app]);

// 5. The agent inside the app answers, and speaks the protocol the app does.
step("Checking the agent inside the app");
const home = mkdtempSync(join(tmpdir(), "bubble-package-home-"));
let info;
try {
  const answer = spawnSync(
    join(resources, "node", "bin", "node"),
    [join(resources, "bubble", "dist", "main.js"), "host", "--stdio"],
    {
      input: '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}\n{"jsonrpc":"2.0","id":2,"method":"shutdown","params":{}}\n',
      env: { ...process.env, BUBBLE_HOME: home },
      encoding: "utf8",
      timeout: 60_000,
    },
  );
  const first = answer.stdout.split("\n").find((line) => line.includes('"id":1'));
  info = first ? JSON.parse(first).result : undefined;
  if (!info) {
    console.error(`the agent did not answer:\n${answer.stdout}\n${answer.stderr}`);
    process.exit(1);
  }
} finally {
  rmSync(home, { recursive: true, force: true });
}
const client = readFileSync(join(native, "crates", "bubble-host", "src", "lib.rs"), "utf8").match(/PROTOCOL_VERSION: u64 = (\d+)/)?.[1];
if (String(info.protocolVersion) !== client) {
  console.error(`the agent speaks protocol ${info.protocolVersion}, the app speaks ${client}`);
  process.exit(1);
}

const size = execFileSync("du", ["-sh", app], { encoding: "utf8" }).split("\t")[0].trim();
console.log(`\n${app}\n  Bubble ${info.bubbleVersion} (protocol ${info.protocolVersion}), Node ${nodeVersion}, ${size}`);
