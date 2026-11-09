#!/usr/bin/env node
// Bubble is a native program; this runs the build for this machine, which
// npm installed as one of the optional dependencies.
"use strict";
const { spawn } = require("child_process");

const platform = `${process.platform}-${process.arch}`;
const name = `@bubblebrain-ai/bubble-${platform}`;
const exe = process.platform === "win32" ? "bubble.exe" : "bubble";

let binary;
try {
  binary = require.resolve(`${name}/bin/${exe}`);
} catch {
  console.error(
    `bubble: there is no build for ${platform} here (${name} is not installed).\n` +
      "Reinstall with optional dependencies enabled: npm install -g @bubblebrain-ai/bubble",
  );
  process.exit(1);
}

const child = spawn(binary, process.argv.slice(2), { stdio: "inherit" });
// Ctrl+C reaches Bubble from the terminal itself; other signals are passed on.
process.on("SIGINT", () => {});
for (const signal of ["SIGTERM", "SIGHUP"]) {
  process.on(signal, () => child.kill(signal));
}
child.on("error", (error) => {
  console.error(`bubble: ${error.message}`);
  process.exit(1);
});
child.on("exit", (code, signal) => {
  if (signal) {
    process.removeAllListeners(signal);
    process.kill(process.pid, signal);
  } else {
    process.exit(code ?? 1);
  }
});
