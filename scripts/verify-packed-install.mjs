import { spawnSync } from "node:child_process";
import { mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const repoRoot = dirname(dirname(fileURLToPath(import.meta.url)));
const sandbox = await mkdtemp(join(tmpdir(), "bubble-packed-install-"));

function run(command, args, cwd) {
  const result = spawnSync(command, args, {
    cwd,
    env: process.env,
    encoding: "utf8",
    stdio: "pipe",
  });
  if (result.status !== 0) {
    process.stderr.write(result.stdout ?? "");
    process.stderr.write(result.stderr ?? "");
    throw new Error(`${command} ${args.join(" ")} failed with exit code ${result.status}`);
  }
  return result;
}

try {
  run("npm", ["pack", "--pack-destination", sandbox], repoRoot);
  const tarballs = (await readdir(sandbox)).filter((file) => file.endsWith(".tgz"));
  if (tarballs.length !== 1) {
    throw new Error(`expected one packed tarball, found ${tarballs.length}`);
  }

  await writeFile(
    join(sandbox, "package.json"),
    JSON.stringify({ name: "bubble-packed-install-smoke", private: true, type: "module" }),
  );
  run(
    "npm",
    ["install", "--no-audit", "--no-fund", join(sandbox, tarballs[0])],
    sandbox,
  );

  const packageRoot = join(sandbox, "node_modules", "@bubblebrain-ai", "bubble");
  const smokeScript = `
    const sdk = await import(${JSON.stringify(join(packageRoot, "dist/sdk/index.js"))});
    if (typeof sdk.BubbleSdk !== "function") throw new Error("BubbleSdk export is unavailable");
    if (typeof sdk.AgentRunInputQueue !== "function") throw new Error("AgentRunInputQueue export is unavailable");
    const client = new sdk.BubbleSdk({ mcp: false });
    for (const method of ["steer", "enqueueTurn", "queueTurn", "clearQueue", "getSessionRunState", "stop"]) {
      if (typeof client[method] !== "function") throw new Error(\`BubbleSdk.\${method} is unavailable\`);
    }

    const tui = await import(${JSON.stringify(
      join(packageRoot, "node_modules/@bubblebrain-ai/pi-tui/dist/src/index.js"),
    )});
    if (typeof tui.Marked !== "function") throw new Error("bundled Pi TUI could not load marked");
  `;
  run(process.execPath, ["--input-type=module", "--eval", smokeScript], sandbox);
  process.stdout.write("Packed Bubble install loaded SDK turn control and the bundled Pi TUI successfully.\n");

  // `npm install -g` keeps every dependency inside the package's own
  // node_modules, so a bundled dependency the tarball lacks is not found
  // anywhere else, as it is in the install above. 0.0.67 was published
  // without get-east-asian-width and did not start.
  const globalPrefix = join(sandbox, "global");
  run("npm", ["install", "-g", "--prefix", globalPrefix, "--no-audit", "--no-fund", join(sandbox, tarballs[0])], sandbox);
  const globalRoot = join(
    globalPrefix,
    ...(process.platform === "win32" ? [] : ["lib"]),
    "node_modules",
    "@bubblebrain-ai",
    "bubble",
  );
  const expectedVersion = JSON.parse(await readFile(join(repoRoot, "package.json"), "utf8")).version;
  const started = spawnSync(process.execPath, [join(globalRoot, "dist", "bin.js"), "--version"], {
    cwd: sandbox,
    env: { ...process.env, BUBBLE_HOME: join(sandbox, "home") },
    encoding: "utf8",
  });
  if (started.status !== 0 || !started.stdout.includes(expectedVersion)) {
    process.stderr.write(started.stdout ?? "");
    process.stderr.write(started.stderr ?? "");
    throw new Error(`globally installed bubble did not start (exit code ${started.status})`);
  }
  process.stdout.write(`Globally installed Bubble started and reported ${expectedVersion}.\n`);
} finally {
  await rm(sandbox, { recursive: true, force: true });
}
