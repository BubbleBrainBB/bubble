/**
 * xAI Grok subscription OAuth login (PKCE + local callback).
 *
 * Mirrors the public OAuth client used by the official Grok CLI so a Grok
 * subscription can drive Bubble's native agent loop the same way ChatGPT
 * OAuth does: browser sign-in once, then bearer + refresh tokens stored in
 * ~/.bubble/auth.json.
 */

import { createServer } from "node:http";
import type { AddressInfo } from "node:net";
import { exec } from "node:child_process";
import { randomBytes, createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import type { OAuthTokens } from "./types.js";
import { OAUTH_REFRESH_TIMEOUT_MS, withOAuthRefreshTimeout } from "./refresh-control.js";
import { chatGptFetch, type ChatGptFetch } from "../network/chatgpt-transport.js";

// Public OAuth client registered for the official Grok CLI ("Grok Build").
const CLIENT_ID = "b1a00492-073a-47ea-816f-4c329264a828";
const AUTH_URL = "https://auth.x.ai/oauth2/authorize";
const TOKEN_URL = "https://auth.x.ai/oauth2/token";
const SCOPE = "openid profile email offline_access grok-cli:access api:access conversations:read conversations:write";

function generatePKCE() {
  const codeVerifier = randomBytes(32).toString("base64url");
  const codeChallenge = createHash("sha256").update(codeVerifier).digest("base64url");
  return { codeVerifier, codeChallenge };
}

function generateOpaqueId() {
  return globalThis.crypto?.randomUUID?.() ?? randomBytes(16).toString("hex");
}

function openBrowser(url: string) {
  const cmd = process.platform === "darwin" ? "open" : process.platform === "win32" ? "start" : "xdg-open";
  exec(`${cmd} ${JSON.stringify(url)}`, () => {
    // ignore errors; we show manual fallback in UI
  });
}

interface CallbackResult {
  code: string;
  state: string;
}

interface CallbackServer {
  /** Loopback URI to hand to the authorization endpoint. Bound and ready. */
  redirectUri: string;
  /** Resolves when the browser hits /callback, rejects on error or timeout. */
  result: Promise<CallbackResult>;
}

/**
 * xAI's OAuth client allows dynamic loopback ports, so listen on an ephemeral
 * port. Awaiting this returns only once the socket is actually bound, so the
 * caller can never build an authorization URL around an empty redirect URI.
 */
async function startCallbackServer(
  timeoutMs: number,
  onStatus?: (msg: string) => void,
): Promise<CallbackServer> {
  let settle: (value: CallbackResult) => void = () => {};
  let fail: (error: Error) => void = () => {};
  const result = new Promise<CallbackResult>((resolve, reject) => {
    settle = resolve;
    fail = reject;
  });
  // Nothing awaits `result` until the caller has a redirect URI, and an early
  // rejection (bind failure) would otherwise be an unhandled rejection.
  result.catch(() => {});

  return new Promise<CallbackServer>((resolveListening, rejectListening) => {
    let resolved = false;
    const finish = (action: () => void) => {
      if (resolved) return;
      resolved = true;
      clearTimeout(timer);
      server.close(action);
    };
    const server = createServer((req, res) => {
      try {
        const port = (server.address() as AddressInfo).port;
        const url = new URL(req.url || "/", `http://127.0.0.1:${port}`);

        if (url.pathname === "/callback") {
          const code = url.searchParams.get("code");
          const state = url.searchParams.get("state");
          const error = url.searchParams.get("error");
          const errorDesc = url.searchParams.get("error_description");

          if (error) {
            res.writeHead(400, { "Content-Type": "text/html" });
            res.end(`<html><body><h1>Authorization failed</h1><p>${errorDesc || error}</p></body></html>`);
            finish(() => fail(new Error(`OAuth error: ${errorDesc || error}`)));
            return;
          }

          if (code) {
            res.writeHead(200, { "Content-Type": "text/html" });
            res.end("<html><body><h1>Authorization received</h1><p>Return to Bubble to check whether sign-in completed. Bubble still needs to exchange and save your credentials.</p></body></html>");
            finish(() => settle({ code, state: state || "" }));
            return;
          }
        }
        res.writeHead(404);
        res.end("Not found");
      } catch {
        res.writeHead(500);
        res.end("Error");
      }
    });

    // Unref'd so a pending login never keeps the process alive on its own,
    // and cleared on completion so a successful login does not leave the
    // event loop holding a five-minute timer.
    const timer = setTimeout(() => {
      finish(() => fail(new Error(`OAuth login timed out after ${timeoutMs / 1000}s`)));
    }, timeoutMs);
    timer.unref?.();

    // Without this, a failed bind (port exhausted, sandbox denies listening)
    // never rejects anything — the login just hangs until the timeout.
    server.on("error", (error) => {
      clearTimeout(timer);
      if (resolved) return;
      resolved = true;
      rejectListening(error instanceof Error ? error : new Error(String(error)));
      fail(error instanceof Error ? error : new Error(String(error)));
    });

    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as AddressInfo).port;
      onStatus?.(`Local server listening on http://127.0.0.1:${port}`);
      // Resolve only once the socket is genuinely bound. The caller used to
      // race this with a single setImmediate tick and declare failure if the
      // listen callback had not fired yet — which it frequently has not, since
      // binding is real async I/O.
      resolveListening({ redirectUri: `http://127.0.0.1:${port}/callback`, result });
    });
  });
}

export interface GrokLoginCallbacks {
  onStatus: (message: string) => void;
}

interface GrokTokenResponse {
  access_token: string;
  refresh_token?: string;
  expires_in: number;
}

function parseTokenResponse(data: GrokTokenResponse, fallbackRefreshToken?: string): OAuthTokens {
  if (!data.access_token || typeof data.access_token !== "string") {
    throw new Error("xAI returned no access token.");
  }
  const refreshToken = data.refresh_token || fallbackRefreshToken;
  if (!refreshToken) {
    throw new Error("xAI returned no refresh token.");
  }
  const expiresIn = typeof data.expires_in === "number" && data.expires_in > 0 ? data.expires_in : 3600;
  return {
    accessToken: data.access_token,
    refreshToken,
    expiresAt: Date.now() + expiresIn * 1000,
  };
}

export async function loginGrok(
  callbacks?: GrokLoginCallbacks,
  options: { fetch?: ChatGptFetch } = {},
): Promise<OAuthTokens> {
  const fetchImpl = options.fetch ?? chatGptFetch;
  callbacks?.onStatus("Starting xAI Grok OAuth login...");

  const pkce = generatePKCE();
  const state = generateOpaqueId();
  const nonce = generateOpaqueId();

  let redirectUri: string;
  let serverPromise: Promise<CallbackResult>;
  try {
    const server = await startCallbackServer(5 * 60 * 1000, callbacks?.onStatus);
    redirectUri = server.redirectUri;
    serverPromise = server.result;
  } catch (error) {
    throw new Error(
      `The local OAuth callback server could not start: ${error instanceof Error ? error.message : String(error)}`,
    );
  }

  const params = new URLSearchParams({
    response_type: "code",
    client_id: CLIENT_ID,
    redirect_uri: redirectUri,
    scope: SCOPE,
    code_challenge: pkce.codeChallenge,
    code_challenge_method: "S256",
    state,
    nonce,
    referrer: "grok-build",
  });
  const authUrl = `${AUTH_URL}?${params.toString()}`;

  callbacks?.onStatus("Opening browser for authorization...");
  openBrowser(authUrl);
  callbacks?.onStatus(`If your browser didn't open, copy this URL manually:\n${authUrl}`);
  callbacks?.onStatus("Waiting for authorization (timeout: 5min)...");

  const { code, state: returnedState } = await serverPromise;
  if (returnedState !== state) {
    throw new Error("OAuth state mismatch. Possible CSRF attack.");
  }

  callbacks?.onStatus("Exchanging authorization code for tokens...");
  const response = await fetchImpl(TOKEN_URL, {
    method: "POST",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: new URLSearchParams({
      grant_type: "authorization_code",
      code,
      redirect_uri: redirectUri,
      client_id: CLIENT_ID,
      code_verifier: pkce.codeVerifier,
    }),
    // The transport waits as long as a model may think; a token endpoint gets no such patience.
    signal: AbortSignal.timeout(OAUTH_REFRESH_TIMEOUT_MS),
  });

  if (!response.ok) {
    const text = await response.text().catch(() => "Unknown error");
    throw new Error(`Token exchange failed: ${response.status} ${response.statusText} - ${text}`);
  }

  const tokens = parseTokenResponse((await response.json()) as GrokTokenResponse);
  callbacks?.onStatus("Login successful!");
  return tokens;
}

export async function refreshGrok(
  refreshToken: string,
  options: { fetch?: ChatGptFetch; timeoutMs?: number } = {},
): Promise<OAuthTokens> {
  return withOAuthRefreshTimeout(async (signal) => {
    const fetchImpl = options.fetch ?? chatGptFetch;
    const response = await fetchImpl(TOKEN_URL, {
      method: "POST",
      signal,
      headers: { "Content-Type": "application/x-www-form-urlencoded" },
      body: new URLSearchParams({
        grant_type: "refresh_token",
        refresh_token: refreshToken,
        client_id: CLIENT_ID,
      }),
    });

    if (!response.ok) {
      const text = await response.text().catch(() => "Unknown error");
      throw new Error(`Token refresh failed: ${response.status} ${response.statusText} - ${text}`);
    }

    return parseTokenResponse((await response.json()) as GrokTokenResponse, refreshToken);
  }, options.timeoutMs);
}

/**
 * One-time credential import from the official Grok CLI's auth store (either
 * Bubble's isolated runtime profile or the user's real ~/.grok). Users who
 * already completed `grok login` skip the browser round-trip entirely.
 */
export function importGrokCliCredentials(
  bubbleHome = join(homedir(), ".bubble"),
  // Injectable so tests can isolate the ~/.grok fallback from the real home
  // directory; production callers rely on the default.
  homeDir = homedir(),
): OAuthTokens | undefined {
  const candidates = [
    join(bubbleHome, "runtimes", "grok", "grok-home", "auth.json"),
    join(homeDir, ".grok", "auth.json"),
  ];
  for (const path of candidates) {
    let parsed: unknown;
    try {
      parsed = JSON.parse(readFileSync(path, "utf-8"));
    } catch {
      continue;
    }
    if (!parsed || typeof parsed !== "object") continue;
    for (const [key, value] of Object.entries(parsed as Record<string, unknown>)) {
      if (!key.startsWith("https://auth.x.ai::")) continue;
      if (!value || typeof value !== "object") continue;
      const entry = value as Record<string, unknown>;
      if (typeof entry.key !== "string" || !entry.key || typeof entry.refresh_token !== "string" || !entry.refresh_token) {
        continue;
      }
      const expiresAt = typeof entry.expires_at === "string" ? Date.parse(entry.expires_at) : NaN;
      return {
        accessToken: entry.key,
        refreshToken: entry.refresh_token,
        // An unparseable expiry just means the first request refreshes.
        expiresAt: Number.isFinite(expiresAt) ? expiresAt : 0,
      };
    }
  }
  return undefined;
}
