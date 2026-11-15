/**
 * Entry point of `bubble host`. stdout carries the protocol and nothing else:
 * anything the libraries print goes to stderr.
 */

import { HostServer } from "./server.js";

export { HostServer, type HostServerOptions } from "./server.js";
export * from "./protocol.js";

export async function runHostCommand(options: { bubbleVersion: string }): Promise<number> {
  const print = (...args: unknown[]) => console.error(...args);
  console.log = print;
  console.info = print;
  console.debug = print;
  const server = new HostServer({
    input: process.stdin,
    output: process.stdout,
    bubbleVersion: options.bubbleVersion,
  });
  // The app closed its end: stop quietly instead of dying on EPIPE.
  process.stdout.on("error", () => process.exit(0));
  await server.run();
  return 0;
}
