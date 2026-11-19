import { mkdtempSync, realpathSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { BubbleSdk, type Provider, type ThinkingLevel } from "../index.js";
import { clearDynamicModelMetadata, getBuiltinModel } from "../../model-catalog.js";

// A turn says how hard it thinks when it starts. That must be the level the
// provider is handed, not the one that was asked for: Gemini 4 has no "off".
let home: string;
let project: string;

beforeEach(() => {
  home = realpathSync(mkdtempSync(join(tmpdir(), "bubble-turn-thinking-home-")));
  project = realpathSync(mkdtempSync(join(tmpdir(), "bubble-turn-thinking-project-")));
  vi.stubEnv("BUBBLE_HOME", home);
});

afterEach(() => {
  // Discovery writes what it learns into a process-wide overlay.
  clearDynamicModelMetadata("google");
  vi.unstubAllEnvs();
  rmSync(home, { recursive: true, force: true });
  rmSync(project, { recursive: true, force: true });
});

/** Runs one turn and returns the level it reported and the level the provider was handed. */
async function levelsOfTurn(
  model: string,
  asked?: unknown,
  prepare?: (sdk: BubbleSdk) => unknown,
): Promise<{ reported: unknown; handed: unknown }> {
  const handed: unknown[] = [];
  const provider: Provider = {
    async *streamChat(_messages, options) {
      handed.push(options?.thinkingLevel);
      yield { type: "text", content: "Done." };
      yield { type: "done" };
    },
    async complete() {
      return "";
    },
  };
  // The SDK reads config.json (and models.json) from BUBBLE_HOME as it starts.
  const sdk = new BubbleSdk({ defaultCwd: project, mcp: false });
  (sdk as any).resolveProvider = () => ({ provider, providerId: model.split(":")[0], model });
  await prepare?.(sdk);
  const session = sdk.createSession({ cwd: project });
  let reported: unknown;
  try {
    for await (const _ of sdk.runTurn(session.id, {
      prompt: "hello",
      ...(asked !== undefined ? { thinkingLevel: asked as ThinkingLevel } : {}),
      onStart: (info) => {
        reported = info.thinkingLevel;
      },
    })) {
    }
  } finally {
    await sdk.deleteSession(session.id);
  }
  expect(handed).toHaveLength(1);
  return { reported, handed: handed[0] };
}

const configure = (config: Record<string, unknown>) => writeFileSync(join(home, "config.json"), JSON.stringify(config));

describe("the level a turn reports is the level it runs at", () => {
  it.each([
    // No "off" and no "minimal" on Gemini 4: both run at low.
    ["google:gemini-4-neon", "off", "low"],
    ["google:gemini-4-neon", "minimal", "low"],
    ["google:gemini-4-argon", "off", "low"],
    // Above a model's ladder: its highest level.
    ["google:gemini-4-neon", "max", "xhigh"],
    ["google:gemini-4-argon", "xhigh", "high"],
    ["google:gemini-4-carbon", "max", "high"],
    // A level the model has is kept.
    ["google:gemini-4-neon", "xhigh", "xhigh"],
    ["google:gemini-4-neon", "medium", "medium"],
    ["google:gemini-3.8-flash", "off", "off"],
    // A Gemini 3 Flash without "off" runs at its lowest level instead.
    ["google:gemini-3-flash-preview", "off", "minimal"],
    ["anthropic:claude-sonnet-4-6", "xhigh", "high"],
    // An explicit level is fitted to the ladder, not swapped for the model's
    // default (medium here), on the providers that would do that themselves.
    ["openai-codex:gpt-5.6-terra", "off", "low"],
  ] as [string, ThinkingLevel, ThinkingLevel][])("%s asked for %s runs at %s", async (model, asked, effective) => {
    expect(await levelsOfTurn(model, asked)).toEqual({ reported: effective, handed: effective });
  });

  it("fits the user's configured default to the model as well", async () => {
    configure({ defaultThinkingLevel: "off" });
    expect(await levelsOfTurn("google:gemini-4-neon")).toEqual({ reported: "low", handed: "low" });
    // The turn's own level wins over the configured one.
    expect(await levelsOfTurn("google:gemini-4-neon", "medium")).toEqual({ reported: "medium", handed: "medium" });
  });

  it("uses the model's default when nothing asks for a level", async () => {
    expect(await levelsOfTurn("google:gemini-4-neon")).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("google:gemini-4-carbon")).toEqual({ reported: "medium", handed: "medium" });
    expect(await levelsOfTurn("test:model")).toEqual({ reported: "off", handed: "off" });
  });

  it.each([null, "banana", 3])("treats a configured level of %j as not set", async (broken) => {
    configure({ defaultThinkingLevel: broken });
    expect(await levelsOfTurn("google:gemini-4-neon")).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("test:model")).toEqual({ reported: "off", handed: "off" });
  });

  it("treats a turn's level that is no level as not given", async () => {
    // What a JSON-RPC caller can send. The configured level is next in line.
    expect(await levelsOfTurn("google:gemini-4-neon", null)).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("test:model", "banana")).toEqual({ reported: "off", handed: "off" });
    configure({ defaultThinkingLevel: "medium" });
    expect(await levelsOfTurn("google:gemini-4-neon", "banana")).toEqual({ reported: "medium", handed: "medium" });
  });

  it("keeps the level asked for on a model the catalog does not know", async () => {
    expect(await levelsOfTurn("test:model", "high")).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("google:gemini-9-unheard-of", "xhigh")).toEqual({ reported: "xhigh", handed: "xhigh" });
  });
});

describe("models a user listed in models.json", () => {
  const listed = ["gemini-4-neon", "gemini-4-hydrogen", "gemini-4-carbon", "custom-model", "gemini-flash-latest"];
  const listModels = async (sdk: BubbleSdk) => {
    const google = sdk.registry.getConfigured().find((p) => p.id === "google")!;
    // With a key the registry would ask Google for its list, were there no list of the user's own.
    expect(google).toMatchObject({ apiKey: "g-key", protocol: "ai-sdk" });
    const fetchMock = vi.fn(async () => new Response("{}", { status: 500 }));
    vi.stubGlobal("fetch", fetchMock);
    try {
      const result = await sdk.registry.discoverModels(google);
      // The list is the user's own: nothing is asked of Google.
      expect(fetchMock).not.toHaveBeenCalled();
      return result;
    } finally {
      vi.unstubAllGlobals();
    }
  };

  beforeEach(() => {
    // models.json is where the provider is read from once it exists, key included.
    writeFileSync(join(home, "models.json"), JSON.stringify({
      providers: { google: { apiKey: "g-key", models: listed.map((id) => ({ id })) } },
    }));
  });

  it("are the catalog's models where the ids match, before and after the list is read", async () => {
    let result: Awaited<ReturnType<typeof listModels>> | undefined;
    const afterListing = async (sdk: BubbleSdk) => {
      result = await listModels(sdk);
    };

    expect(await levelsOfTurn("google:gemini-4-neon")).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("google:gemini-4-neon", undefined, afterListing)).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("google:gemini-4-hydrogen", undefined, afterListing)).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("google:gemini-4-carbon", undefined, afterListing)).toEqual({ reported: "medium", handed: "medium" });
    expect(await levelsOfTurn("google:gemini-4-neon", "off", afterListing)).toEqual({ reported: "low", handed: "low" });

    expect(result!.models.map((model) => model.id)).toEqual(listed);
    expect(result!.models[0]).toMatchObject({
      id: "gemini-4-neon",
      reasoningLevels: ["low", "medium", "high", "xhigh"],
      defaultReasoningLevel: "high",
      contextWindow: 1_048_576,
      tier: "strong",
    });
  });

  it("keep the level asked for when Bubble knows nothing about them, whether or not the list was read", async () => {
    expect(await levelsOfTurn("google:custom-model", "high")).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("google:custom-model", "high", async (sdk) => {
      await listModels(sdk);
      // Listed, so selectable in a picker, on a ladder that is only a placeholder.
      expect(getBuiltinModel("google", "custom-model")).toMatchObject({ reasoningLevels: ["off"], reasoningLevelsAssumed: true });
      expect(getBuiltinModel("google", "gemini-4-neon")?.reasoningLevelsAssumed).toBeUndefined();
    })).toEqual({ reported: "high", handed: "high" });
  });

  it("make no claim about a Gemini id that names no version", async () => {
    // "gemini-flash-latest" is nothing Bubble lists itself; written into
    // models.json it is the user's model, run at the level they ask for.
    expect(await levelsOfTurn("google:gemini-flash-latest", "high")).toEqual({ reported: "high", handed: "high" });
    expect(await levelsOfTurn("google:gemini-flash-latest", "high", async (sdk) => {
      const result = await listModels(sdk);
      expect(result.models.find((model) => model.id === "gemini-flash-latest")?.reasoningLevels).toBeUndefined();
      expect(getBuiltinModel("google", "gemini-flash-latest")).toMatchObject({ reasoningLevels: ["off"], reasoningLevelsAssumed: true });
    })).toEqual({ reported: "high", handed: "high" });
    // Nothing asked for and nothing known: no thinking level is sent for it.
    expect(await levelsOfTurn("google:gemini-flash-latest", undefined, listModels)).toEqual({ reported: "off", handed: "off" });
  });
});
