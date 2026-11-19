import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { afterEach, describe, expect, it } from "vitest";
import {
  effectiveThemeModeForTerminal,
  shouldProbeTerminalTheme,
  UserConfig,
} from "../config.js";

describe("UserConfig", () => {
  const root = join(tmpdir(), `bubble-config-test-${Date.now()}`);
  mkdirSync(root, { recursive: true });
  const originalBubbleHome = process.env.BUBBLE_HOME;

  afterEach(() => {
    process.env.BUBBLE_HOME = originalBubbleHome;
  });

  it("reads a default thinking level only when it is one", () => {
    process.env.BUBBLE_HOME = root;
    const load = (defaultThinkingLevel: unknown) => {
      writeFileSync(join(root, "config.json"), JSON.stringify({ defaultThinkingLevel }));
      return new UserConfig().getDefaultThinkingLevel();
    };

    expect(load("xhigh")).toBe("xhigh");
    expect(load("off")).toBe("off");
    // null is what a cleared setting leaves behind in JSON; it is not a level.
    expect(load(null)).toBeUndefined();
    expect(load("banana")).toBeUndefined();
    expect(load(3)).toBeUndefined();
  });

  it("persists disabled Skill names as a normalized user preference", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(join(root, "config.json"), JSON.stringify({}, null, 2));

    const config = new UserConfig();
    config.setDisabledSkills(["podcast", "repo-review", "podcast", "bad name"]);

    expect(new UserConfig().getDisabledSkills()).toEqual(["podcast", "repo-review"]);
    config.setDisabledSkills([]);
    expect(new UserConfig().getDisabledSkills()).toEqual([]);
  });

  it("falls back to the most recent model when defaultModel is missing", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(
      join(root, "config.json"),
      JSON.stringify({
        recentModels: ["openai:gpt-5.4"],
      }, null, 2),
    );

    const config = new UserConfig();
    expect(config.getDefaultModel()).toBe("openai:gpt-5.4");
  });

  it("updates defaultModel when pushing a recent model", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(join(root, "config.json"), JSON.stringify({}, null, 2));

    const config = new UserConfig();
    config.pushRecentModel("openai:gpt-5.4");

    expect(config.getDefaultModel()).toBe("openai:gpt-5.4");
    expect(config.getRecentModels()[0]).toBe("openai:gpt-5.4");
  });

  it("persists and restores default thinking level", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(join(root, "config.json"), JSON.stringify({}, null, 2));

    const config = new UserConfig();
    config.setDefaultThinkingLevel("high");

    const restored = new UserConfig();
    expect(restored.getDefaultThinkingLevel()).toBe("high");
  });

  it("preserves OpenRouter provider and model selections", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(
      join(root, "config.json"),
      JSON.stringify({
        providers: [{
          id: "openrouter",
          name: "OpenRouter",
          baseURL: "https://openrouter.ai/api/v1",
          apiKey: "test-key",
          enabled: true,
        }],
        defaultProvider: "openrouter",
        defaultModel: "openrouter:stealth/ox-alpha",
        recentModels: ["openrouter:stealth/ox-alpha"],
      }, null, 2),
    );

    const config = new UserConfig();
    expect(config.getProviders().map((provider) => provider.id)).toEqual(["openrouter"]);
    expect(config.getDefaultProvider()).toBe("openrouter");
    expect(config.getDefaultModel()).toBe("openrouter:stealth/ox-alpha");
    expect(config.getRecentModels()).toEqual(["openrouter:stealth/ox-alpha"]);
  });

  it("drops OpenRouter model selections outside the fixed Ox policy", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(
      join(root, "config.json"),
      JSON.stringify({
        defaultProvider: "openrouter",
        defaultModel: "openrouter:some/other-model",
        recentModels: [
          "openrouter:some/other-model",
          "openrouter:stealth/ox-alpha",
          "openai:gpt-5.5",
        ],
      }, null, 2),
    );

    const config = new UserConfig();
    expect(config.getDefaultProvider()).toBe("openrouter");
    expect(config.getDefaultModel()).toBe("openrouter:stealth/ox-alpha");
    expect(config.getRecentModels()).toEqual([
      "openrouter:stealth/ox-alpha",
      "openai:gpt-5.5",
    ]);
  });

  it("migrates legacy flat theme overrides into the dark mode bucket", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(
      join(root, "config.json"),
      JSON.stringify({
        theme: {
          messageUserText: "#ffffff",
          toolError: "#ff0000",
          ignored: 42,
        },
      }, null, 2),
    );

    const config = new UserConfig();
    expect(config.getTheme()).toEqual({
      mode: "dark",
      overrides: {
        messageUserText: "#ffffff",
        toolError: "#ff0000",
      },
    });
    expect(config.getThemeOverrides()).toEqual({
      messageUserText: "#ffffff",
      toolError: "#ff0000",
    });
  });

  it("loads explicit theme mode + overrides", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(
      join(root, "config.json"),
      JSON.stringify({
        theme: {
          mode: "light",
          overrides: { accent: "#123456" },
        },
      }, null, 2),
    );

    const config = new UserConfig();
    expect(config.getThemeMode()).toBe("light");
    expect(config.getThemeOverrides()).toEqual({ accent: "#123456" });
  });

  it("accepts a bare theme-mode string", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(
      join(root, "config.json"),
      JSON.stringify({ theme: "light" }, null, 2),
    );

    const config = new UserConfig();
    expect(config.getThemeMode()).toBe("light");
    expect(config.getTheme()).toEqual({ mode: "light", explicit: true });
    expect(config.getThemeOverrides()).toEqual({});
  });

  it("defaults theme mode to auto when missing", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(join(root, "config.json"), JSON.stringify({}, null, 2));

    const config = new UserConfig();
    expect(config.getThemeMode()).toBe("auto");
  });

  it("treats legacy bare dark theme as terminal-auto on light terminals", () => {
    const legacy = { mode: "dark" as const };

    expect(shouldProbeTerminalTheme(legacy)).toBe(true);
    expect(effectiveThemeModeForTerminal(legacy, "light")).toBe("auto");
    expect(effectiveThemeModeForTerminal(legacy, "dark")).toBe("dark");
  });

  it("preserves explicitly selected dark theme", () => {
    const explicit = { mode: "dark" as const, explicit: true };

    // Probing still happens for forced themes: the terminal background is
    // needed to decide whether the forced theme's canvas must be painted.
    expect(shouldProbeTerminalTheme(explicit)).toBe(true);
    expect(effectiveThemeModeForTerminal(explicit, "light")).toBe("dark");
  });

  it("marks theme mode changes explicit", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(join(root, "config.json"), JSON.stringify({}, null, 2));

    const config = new UserConfig();
    config.setThemeMode("dark");

    expect(config.getTheme()).toEqual({ mode: "dark", explicit: true });
  });

  it("loads sanitized subagent category overrides", () => {
    process.env.BUBBLE_HOME = root;
    writeFileSync(
      join(root, "config.json"),
      JSON.stringify({
        agentCategories: {
          Review: {
            model: "openai:gpt-5.4",
            thinkingLevel: "high",
            maxConcurrent: 2.9,
            ignored: true,
          },
          broken: "skip",
        },
      }, null, 2),
    );

    const config = new UserConfig();

    expect(config.getAgentCategories()).toEqual({
      review: {
        model: "openai:gpt-5.4",
        thinkingLevel: "high",
        maxConcurrent: 2,
      },
    });
  });
});
