import type { ReasoningEffort } from "./types.js";
import { OPENROUTER_MODEL_ID } from "./provider-model-policy.js";

export type ProviderProtocol = "openai-chat" | "openai-responses" | "anthropic-messages" | "ark-responses" | "ai-sdk";

export interface BuiltinProviderDefinition {
  id: string;
  name: string;
  baseURL: string;
  protocol?: ProviderProtocol;
  hidden?: boolean;
  supportsOAuth?: boolean;
}

/**
 * Relative capability/cost tier within one provider's lineup (never a
 * cross-provider comparison). Drives rank-guarded category routing and the
 * routing menu (docs/model-routing-design.md §2). Untiered models never
 * participate in automatic routing but stay reachable by explicit name.
 */
export type ModelTier = "fast" | "balanced" | "strong";

export interface BuiltinModelDefinition {
  id: string;
  name: string;
  providerId: string;
  reasoningLevels: ReasoningEffort[];
  defaultReasoningLevel?: ReasoningEffort;
  tier?: ModelTier;
  contextWindow?: number;
  /** Routes this model through the Codex Responses Lite backend when true. */
  useResponsesLite?: boolean;
  /**
   * Server-declared cap on per-tool-output tokens. When set, the agent must
   * truncate each tool result to this token budget before adding it to history
   * — otherwise the server's input window is exceeded by raw tool dumps.
   * (For codex models this comes from the API's `truncation_policy.limit`.)
   */
  toolOutputTokenLimit?: number;
}

export const BUILTIN_PROVIDERS: BuiltinProviderDefinition[] = [
  { id: "openrouter", name: "OpenRouter", baseURL: "https://openrouter.ai/api/v1" },
  { id: "openai", name: "OpenAI", baseURL: "https://api.openai.com/v1", supportsOAuth: true },
  { id: "openai-codex", name: "OpenAI Codex (ChatGPT)", baseURL: "https://chatgpt.com/backend-api" },
  { id: "opencode-zen", name: "OpenCode Zen", baseURL: "https://opencode.ai/zen/v1", protocol: "openai-responses" },
  // Grok subscription models via the CLI chat proxy. OAuth-only: the proxy
  // accepts xAI session bearers (from /login grok), not api.x.ai API keys.
  { id: "grok", name: "Grok Subscription", baseURL: "https://cli-chat-proxy.grok.com/v1", supportsOAuth: true },
  { id: "anthropic", name: "Anthropic", baseURL: "https://api.anthropic.com", protocol: "anthropic-messages" },
  { id: "deepseek", name: "DeepSeek", baseURL: "https://api.deepseek.com" },
  // Native Gemini API via the AI SDK google provider. Users who configured the
  // old OpenAI-compat endpoint can keep it by setting protocol "openai-chat"
  // and the /openai baseURL explicitly in models.json.
  { id: "google", name: "Google Gemini", baseURL: "https://generativelanguage.googleapis.com/v1beta", protocol: "ai-sdk" },
  { id: "zhipuai", name: "Zhipu AI", baseURL: "https://open.bigmodel.cn/api/paas/v4" },
  { id: "zhipuai-coding-plan", name: "Zhipu AI Coding Plan", baseURL: "https://open.bigmodel.cn/api/coding/paas/v4" },
  { id: "zai", name: "Z.AI", baseURL: "https://api.z.ai/api/paas/v4" },
  { id: "zai-coding-plan", name: "Z.AI Coding Plan", baseURL: "https://api.z.ai/api/coding/paas/v4" },
  { id: "alibaba", name: "Alibaba DashScope", baseURL: "https://dashscope.aliyuncs.com/compatible-mode/v1" },
  // Bailian token plan: its own endpoint and quota, separate from the
  // pay-per-token DashScope provider above. Hosts several vendors' models.
  { id: "bailian-token-plan", name: "阿里云百炼 Token Plan", baseURL: "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1" },
  { id: "doubao", name: "Doubao (Volcengine Ark)", baseURL: "https://ark.cn-beijing.volces.com/api/v3", protocol: "ark-responses" },
  { id: "minimax", name: "MiniMax Token Plan", baseURL: "https://api.minimaxi.com/anthropic", protocol: "anthropic-messages" },
  { id: "minimax-anthropic", name: "MiniMax API", baseURL: "https://api.minimaxi.com/anthropic", protocol: "anthropic-messages" },
  { id: "mimo-token-plan", name: "Xiaomi MiMo Token Plan", baseURL: "https://token-plan-cn.xiaomimimo.com/anthropic", protocol: "anthropic-messages" },
  { id: "stepfun", name: "StepFun Step Plan", baseURL: "https://api.stepfun.com/step_plan/v1" },
  { id: "stepfun-api", name: "StepFun API", baseURL: "https://api.stepfun.com/v1", protocol: "openai-chat" },
  { id: "moonshot-cn", name: "Moonshot (国内 platform.moonshot.cn)", baseURL: "https://api.moonshot.cn/v1" },
  { id: "moonshot-intl", name: "Moonshot (海外 platform.moonshot.ai)", baseURL: "https://api.moonshot.ai/v1" },
  { id: "kimi-for-coding", name: "Kimi for Coding", baseURL: "https://api.kimi.com/coding/v1" },
  { id: "groq", name: "Groq", baseURL: "https://api.groq.com/openai/v1" },
  { id: "together", name: "Together AI", baseURL: "https://api.together.xyz/v1" },
  { id: "fireworks", name: "Fireworks", baseURL: "https://api.fireworks.ai/inference/v1" },
  { id: "local", name: "Local (OpenAI-compatible)", baseURL: "http://localhost:11434/v1" },
];

const ALL_OPENAI_LEVELS: ReasoningEffort[] = ["off", "minimal", "low", "medium", "high", "xhigh"];
const GPT56_LEVELS: ReasoningEffort[] = ["low", "medium", "high", "xhigh", "max", "ultra"];
const GPT56_LUNA_LEVELS: ReasoningEffort[] = ["low", "medium", "high", "xhigh", "max"];
const GPT51_LEVELS: ReasoningEffort[] = ["off", "low", "medium", "high"];
const GPT51_CODEX_MAX_LEVELS: ReasoningEffort[] = ["off", "low", "medium", "high", "xhigh"];
const GPT51_CODEX_MINI_LEVELS: ReasoningEffort[] = ["off", "medium", "high"];
const OPENAI_CHAT_LEVELS: ReasoningEffort[] = ["off"];
const MUSE_SPARK_LEVELS: ReasoningEffort[] = ["minimal", "low", "medium", "high", "xhigh"];
// Ox Alpha declares mandatory reasoning with exactly low/high/max through
// OpenRouter's live model metadata. The endpoint rejects disabled reasoning.
const OPENROUTER_OX_REASONING_LEVELS: ReasoningEffort[] = ["low", "high", "max"];
// Internal representation for APIs that expose thinking as enabled/disabled.
// UI code must render supported toggle models as on/off, not as "medium" effort.
const TOGGLE_THINKING_LEVELS: ReasoningEffort[] = ["off", "medium"];
// GLM-5.2 accepts OpenAI-style `reasoning_effort`. We expose high and max plus
// "off", which disables thinking outright via `thinking: {type: "disabled"}`.
const GLM_5_2_LEVELS: ReasoningEffort[] = ["high", "max", "off"];
// The GLM-5.3 family normalizes efforts into exactly three real tiers. Its
// default is max, and disabling thinking maps to low rather than turning it off.
const GLM_5_3_LEVELS: ReasoningEffort[] = ["low", "high", "max"];
// Kimi K2.7 Code variants only support thinking mode (disabling it errors), so
// "off" is not offered — the model is always in its thinking variant.
const KIMI_THINKING_ONLY_LEVELS: ReasoningEffort[] = ["medium"];
// Kimi K3 takes OpenAI-style reasoning_effort (low/high/max — no "medium"),
// verified against the live endpoint: low spent 92 reasoning tokens on a task
// where high spent 1310. Thinking is disableable on k3 (thinking.type
// "disabled"); the 256k variant exposes effort only, so it has no "off".
// K3.1 Pro (and its highspeed slot) is effort-only too: GET /coding/v1/models
// reports reasoning.type "only" and the endpoint 400s on thinking disabled.
// The default of "high" is Bubble's choice and matches the coding endpoint's
// reported default; Moonshot's open platform defaults to max server-side.
const KIMI_K3_LEVELS: ReasoningEffort[] = ["off", "low", "high", "max"];
const KIMI_K3_EFFORT_ONLY_LEVELS: ReasoningEffort[] = ["low", "high", "max"];
// DashScope's Moonshot passthrough (kimi/ ids) documents K3 as thinking-only
// with reasoning_effort "max" as its sole value. Not live-verified: the probing
// account has the passthrough product disabled.
const KIMI_PASSTHROUGH_K3_LEVELS: ReasoningEffort[] = ["max"];
// DeepSeek V4 defaults to thinking/high, but the OpenAI-compatible API also
// supports disabling thinking and selecting low/high/max effort explicitly.
const DEEPSEEK_V4_LEVELS: ReasoningEffort[] = ["off", "low", "high", "max"];
// Bailian token plan grades, read off the endpoint's own 400 responses
// ("'reasoning_effort' must be one of: ..."), which differ per model. "none"
// maps to our "off". qwen3.6-flash / qwen3.7-* stop at xhigh; deepseek-v4-pro
// there has no off switch.
const BAILIAN_FULL_LEVELS: ReasoningEffort[] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
const BAILIAN_NO_MAX_LEVELS: ReasoningEffort[] = ["off", "minimal", "low", "medium", "high", "xhigh"];
const BAILIAN_DEEPSEEK_LEVELS: ReasoningEffort[] = ["low", "medium", "high", "xhigh", "max"];
const STEPFUN_REASONING_LEVELS: ReasoningEffort[] = ["off", "low", "medium", "high"];
const DOUBAO_SEED_REASONING_LEVELS: ReasoningEffort[] = ["minimal", "low", "medium", "high"];
const MINIMAX_M3_REASONING_LEVELS: ReasoningEffort[] = ["off", "medium"];
const MINIMAX_REASONING_LEVELS: ReasoningEffort[] = ["medium"];
// Anthropic exposes reasoning depth through output_config.effort (low | medium
// | high | xhigh | max), not a token budget. Sonnet 5 also supports xhigh.
// Fable 5+ and Opus 5.5 have thinking always on, so they have no "off".
const ANTHROPIC_OPUS_EFFORT_LEVELS: ReasoningEffort[] = ["off", "low", "medium", "high", "xhigh", "max"];
const ANTHROPIC_SONNET_EFFORT_LEVELS: ReasoningEffort[] = ["off", "low", "medium", "high", "max"];
const ANTHROPIC_FABLE_EFFORT_LEVELS: ReasoningEffort[] = ["low", "medium", "high", "xhigh", "max"];
const ANTHROPIC_CHAT_LEVELS: ReasoningEffort[] = ["off"];
// Grok 4.5 exposes low/medium/high effort with no "off" (thinking always on);
// the composer model declares no effort support at all.
const GROK_45_LEVELS: ReasoningEffort[] = ["low", "medium", "high"];
// Gemini 4 documents only low/medium (default)/high. Verified live on
// 2026-10-13: thinkingLevel=minimal returns 400, so there is no off/minimal.
const GEMINI_4_LEVELS: ReasoningEffort[] = ["low", "medium", "high"];
const GEMINI_3_LEVELS: ReasoningEffort[] = ["low", "medium", "high"];
const GEMINI_3_FLASH_LEVELS: ReasoningEffort[] = ["minimal", "low", "medium", "high"];
// Verified against the native API on 2026-09-02: Gemini 3.7/3.8 Flash reject
// thinkingLevel=minimal but accept thinkingBudget=0 plus low/medium/high.
const GEMINI_37_38_FLASH_LEVELS: ReasoningEffort[] = ["off", "low", "medium", "high"];
const GEMINI_25_PRO_LEVELS: ReasoningEffort[] = ["low", "medium", "high"];
const GEMINI_25_FLASH_LEVELS: ReasoningEffort[] = ["off", "low", "medium", "high"];

export const BUILTIN_MODELS: BuiltinModelDefinition[] = [
  // OpenRouter exposes a very large catalog, but Bubble intentionally offers
  // only Ox Alpha. Discovery refreshes this entry's public metadata without
  // expanding provider membership.
  { id: OPENROUTER_MODEL_ID, name: "Ox Alpha", providerId: "openrouter", tier: "strong", reasoningLevels: OPENROUTER_OX_REASONING_LEVELS, defaultReasoningLevel: "max", contextWindow: 1048576 },

  // Verified against /codex/models on 2026-09-17: same request shape as gpt-5.6-terra
  // (responses-lite, six effort levels, 10k tool-output cap); minimal_client_version 0.153.0.
  { id: "gpt-6-astra", name: "GPT-6-Astra", providerId: "openai-codex", tier: "strong", reasoningLevels: GPT56_LEVELS, defaultReasoningLevel: "medium", contextWindow: 372000, useResponsesLite: true, toolOutputTokenLimit: 10000 },
  { id: "gpt-5.6-sol", name: "GPT-5.6-Sol", providerId: "openai-codex", tier: "balanced", reasoningLevels: GPT56_LEVELS, defaultReasoningLevel: "low", contextWindow: 372000, useResponsesLite: true, toolOutputTokenLimit: 10000 },
  { id: "gpt-5.6-terra", name: "GPT-5.6-Terra", providerId: "openai-codex", tier: "strong", reasoningLevels: GPT56_LEVELS, defaultReasoningLevel: "medium", contextWindow: 372000, useResponsesLite: true, toolOutputTokenLimit: 10000 },
  { id: "gpt-5.6-luna", name: "GPT-5.6-Luna", providerId: "openai-codex", tier: "strong", reasoningLevels: GPT56_LUNA_LEVELS, defaultReasoningLevel: "medium", contextWindow: 372000, useResponsesLite: true, toolOutputTokenLimit: 10000 },
  { id: "gpt-5.5", name: "gpt-5.5", providerId: "openai-codex", tier: "strong", reasoningLevels: ALL_OPENAI_LEVELS, contextWindow: 272000, toolOutputTokenLimit: 10000 },
  { id: "gpt-5.4", name: "gpt-5.4", providerId: "openai-codex", tier: "balanced", reasoningLevels: ALL_OPENAI_LEVELS, contextWindow: 272000 },
  { id: "gpt-5.4-mini", name: "gpt-5.4-mini", providerId: "openai-codex", tier: "fast", reasoningLevels: ALL_OPENAI_LEVELS, contextWindow: 272000 },
  { id: "gpt-5.3-codex", name: "gpt-5.3-codex", providerId: "openai-codex", reasoningLevels: ALL_OPENAI_LEVELS, contextWindow: 272000 },
  { id: "gpt-5.3-codex-spark", name: "gpt-5.3-codex-spark", providerId: "openai-codex", reasoningLevels: ALL_OPENAI_LEVELS, contextWindow: 272000 },
  { id: "gpt-5.2-codex", name: "gpt-5.2-codex", providerId: "openai-codex", reasoningLevels: ALL_OPENAI_LEVELS, contextWindow: 272000 },
  { id: "gpt-5.2", name: "gpt-5.2", providerId: "openai-codex", reasoningLevels: ALL_OPENAI_LEVELS, contextWindow: 272000 },
  { id: "gpt-5.1-codex-max", name: "gpt-5.1-codex-max", providerId: "openai-codex", reasoningLevels: GPT51_CODEX_MAX_LEVELS, contextWindow: 272000 },
  { id: "gpt-5.1-codex-mini", name: "gpt-5.1-codex-mini", providerId: "openai-codex", tier: "fast", reasoningLevels: GPT51_CODEX_MINI_LEVELS, contextWindow: 272000 },
  { id: "gpt-5.1", name: "gpt-5.1", providerId: "openai-codex", reasoningLevels: GPT51_LEVELS, contextWindow: 272000 },

  { id: "grok-4.5", name: "Grok 4.5", providerId: "grok", tier: "strong", reasoningLevels: GROK_45_LEVELS, defaultReasoningLevel: "high", contextWindow: 500000 },
  { id: "grok-composer-2.5-fast", name: "Grok Composer 2.5 Fast", providerId: "grok", tier: "fast", reasoningLevels: OPENAI_CHAT_LEVELS, contextWindow: 200000 },

  { id: "gpt-4o", name: "gpt-4o", providerId: "openai", tier: "balanced", reasoningLevels: OPENAI_CHAT_LEVELS, contextWindow: 128000 },
  { id: "gpt-4o-mini", name: "gpt-4o-mini", providerId: "openai", tier: "fast", reasoningLevels: OPENAI_CHAT_LEVELS, contextWindow: 128000 },
  { id: "o1-preview", name: "o1-preview", providerId: "openai", tier: "strong", reasoningLevels: ["off", "low", "medium", "high"], contextWindow: 128000 },
  { id: "o1-mini", name: "o1-mini", providerId: "openai", tier: "fast", reasoningLevels: ["off", "low", "medium", "high"], contextWindow: 128000 },
  { id: "gpt-4-turbo", name: "gpt-4-turbo", providerId: "openai", reasoningLevels: OPENAI_CHAT_LEVELS, contextWindow: 128000 },

  { id: "muse-spark-1.3-contributor-free", name: "Muse Spark 1.3 Free", providerId: "opencode-zen", reasoningLevels: MUSE_SPARK_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "muse-spark-1.2", name: "Muse Spark 1.2", providerId: "opencode-zen", reasoningLevels: MUSE_SPARK_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "muse-spark-1.2-contributor-free", name: "Muse Spark 1.2 Free", providerId: "opencode-zen", reasoningLevels: MUSE_SPARK_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  // Zen /chat/completions; models.dev/opencode metadata checked 2026-09-26.
  { id: "space-bunny-free", name: "Space Bunny Free", providerId: "opencode-zen", reasoningLevels: ["low", "medium", "high", "xhigh", "max"], defaultReasoningLevel: "high", contextWindow: 1048576 },

  // Current public lineup, verified against platform.claude.com on 2026-09-23.
  { id: "claude-fable-5-1", name: "Claude Fable 5.1", providerId: "anthropic", tier: "strong", reasoningLevels: ANTHROPIC_FABLE_EFFORT_LEVELS, defaultReasoningLevel: "high", contextWindow: 1000000 },
  { id: "claude-opus-5-5", name: "Claude Opus 5.5", providerId: "anthropic", tier: "strong", reasoningLevels: ANTHROPIC_FABLE_EFFORT_LEVELS, defaultReasoningLevel: "medium", contextWindow: 1000000 },
  { id: "claude-sonnet-5", name: "Claude Sonnet 5", providerId: "anthropic", tier: "balanced", reasoningLevels: ANTHROPIC_OPUS_EFFORT_LEVELS, defaultReasoningLevel: "high", contextWindow: 1000000 },
  { id: "claude-opus-5", name: "Claude Opus 5", providerId: "anthropic", tier: "strong", reasoningLevels: ANTHROPIC_OPUS_EFFORT_LEVELS, defaultReasoningLevel: "high", contextWindow: 1000000 },
  { id: "claude-fable-5", name: "Claude Fable 5", providerId: "anthropic", tier: "strong", reasoningLevels: ANTHROPIC_FABLE_EFFORT_LEVELS, defaultReasoningLevel: "high", contextWindow: 1000000 },
  { id: "claude-opus-4-8", name: "Claude Opus 4.8", providerId: "anthropic", tier: "strong", reasoningLevels: ANTHROPIC_OPUS_EFFORT_LEVELS, defaultReasoningLevel: "high", contextWindow: 1000000 },
  { id: "claude-sonnet-4-6", name: "Claude Sonnet 4.6", providerId: "anthropic", tier: "balanced", reasoningLevels: ANTHROPIC_SONNET_EFFORT_LEVELS, defaultReasoningLevel: "high", contextWindow: 1000000 },
  { id: "claude-haiku-4-5-20251001", name: "Claude Haiku 4.5", providerId: "anthropic", tier: "fast", reasoningLevels: ANTHROPIC_CHAT_LEVELS, contextWindow: 200000 },

  { id: "deepseek-v4-flash", name: "deepseek-v4-flash", providerId: "deepseek", tier: "fast", reasoningLevels: DEEPSEEK_V4_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  // V4.1 Flash (2026-09-10): high is the default, not the only effort.
  // https://api-docs.deepseek.com/guides/thinking_mode
  { id: "deepseek-flash", name: "deepseek-flash", providerId: "deepseek", reasoningLevels: DEEPSEEK_V4_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  // Legacy vision alias now routes to V4.1 Flash. Keep its explicit-only
  // routing policy; existing sessions can continue using the old model ID.
  { id: "deepseek-v4-flash-vision-exp", name: "DeepSeek-V4-Flash-Vision-Exp", providerId: "deepseek", reasoningLevels: DEEPSEEK_V4_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "deepseek-v4-pro", name: "deepseek-v4-pro", providerId: "deepseek", tier: "strong", reasoningLevels: DEEPSEEK_V4_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  // Offline/no-key fallback only: with an API key the registry replaces this
  // list via fetchGeminiModels (GET /v1beta/models, newest five). Gemini 3
  // thinking support varies by release: 3.7/3.8 Flash support off/low/medium/high
  // while older Flash releases expose minimal/low/medium/high.
  // Gemini 4 Argon: 1,048,576-token window shared by input, thinking and output.
  // https://ai.google.dev/gemini-api/docs/models/gemini-4-argon
  { id: "gemini-4-argon", name: "Gemini 4 Argon", providerId: "google", tier: "strong", reasoningLevels: GEMINI_4_LEVELS, defaultReasoningLevel: "medium", contextWindow: 1048576 },
  { id: "gemini-3.8-flash", name: "Gemini 3.8 Flash", providerId: "google", tier: "fast", reasoningLevels: GEMINI_37_38_FLASH_LEVELS, contextWindow: 1048576 },
  { id: "gemini-3.7-flash", name: "Gemini 3.7 Flash", providerId: "google", tier: "fast", reasoningLevels: GEMINI_37_38_FLASH_LEVELS, contextWindow: 1048576 },
  { id: "gemini-3.5-flash", name: "Gemini 3.5 Flash", providerId: "google", tier: "fast", reasoningLevels: GEMINI_3_FLASH_LEVELS, contextWindow: 1048576 },
  { id: "gemini-3.1-pro-preview", name: "Gemini 3.1 Pro", providerId: "google", tier: "strong", reasoningLevels: GEMINI_3_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "gemini-3-flash-preview", name: "Gemini 3 Flash", providerId: "google", tier: "fast", reasoningLevels: GEMINI_3_FLASH_LEVELS, contextWindow: 1048576 },
  { id: "gemini-2.5-pro", name: "Gemini 2.5 Pro", providerId: "google", tier: "strong", reasoningLevels: GEMINI_25_PRO_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "gemini-2.5-flash", name: "Gemini 2.5 Flash", providerId: "google", tier: "fast", reasoningLevels: GEMINI_25_FLASH_LEVELS, contextWindow: 1048576 },
  { id: "glm-5.2", name: "GLM-5.2", providerId: "zhipuai", tier: "strong", reasoningLevels: GLM_5_2_LEVELS, contextWindow: 1000000 },
  { id: "glm-5.1", name: "GLM-5.1", providerId: "zhipuai", tier: "balanced", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 200000 },
  { id: "glm-4.7", name: "GLM-4.7", providerId: "zhipuai", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 204800 },
  { id: "glm-4.6", name: "GLM-4.6", providerId: "zhipuai", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 204800 },
  // GLM-5.3-Flash is a native multimodal model with the same mandatory
  // low/high/max thinking contract as GLM-5.3. It is fully available on the
  // Coding Plan and GET /models (verified live 2026-08-26). Official docs list
  // a 1M context window and position it as the fast/low-cost plan model.
  { id: "glm-5.3-flash", name: "GLM-5.3-Flash", providerId: "zhipuai-coding-plan", tier: "fast", reasoningLevels: GLM_5_3_LEVELS, defaultReasoningLevel: "max", contextWindow: 1000000 },
  // Coding Plan exposed GLM-5.3 before GET /models listed it (verified live
  // 2026-08-14). The standard API was still marked "coming soon" at that time.
  { id: "glm-5.3", name: "GLM-5.3", providerId: "zhipuai-coding-plan", tier: "strong", reasoningLevels: GLM_5_3_LEVELS, defaultReasoningLevel: "max", contextWindow: 1000000 },
  { id: "glm-5.2", name: "GLM-5.2", providerId: "zhipuai-coding-plan", tier: "strong", reasoningLevels: GLM_5_2_LEVELS, contextWindow: 1000000 },
  { id: "glm-5.1", name: "GLM-5.1", providerId: "zhipuai-coding-plan", tier: "balanced", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 200000 },
  { id: "glm-4.7", name: "GLM-4.7", providerId: "zhipuai-coding-plan", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 204800 },
  { id: "glm-4.6", name: "GLM-4.6", providerId: "zhipuai-coding-plan", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 204800 },
  { id: "glm-5.2", name: "GLM-5.2", providerId: "zai", tier: "strong", reasoningLevels: GLM_5_2_LEVELS, contextWindow: 1000000 },
  { id: "glm-5.1", name: "GLM-5.1", providerId: "zai", tier: "balanced", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 200000 },
  { id: "glm-4.7", name: "GLM-4.7", providerId: "zai", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 204800 },
  { id: "glm-4.6", name: "GLM-4.6", providerId: "zai", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 204800 },
  { id: "glm-5.2", name: "GLM-5.2", providerId: "zai-coding-plan", tier: "strong", reasoningLevels: GLM_5_2_LEVELS, contextWindow: 1000000 },
  { id: "glm-5-turbo", name: "GLM-5-Turbo", providerId: "zai-coding-plan", tier: "fast", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 200000 },
  { id: "glm-4.7", name: "GLM-4.7", providerId: "zai-coding-plan", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 204800 },
  { id: "glm-4.6", name: "GLM-4.6", providerId: "zai-coding-plan", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 200000 },
  { id: "qwen3.6-plus", name: "Qwen3.6 Plus", providerId: "alibaba", tier: "balanced", reasoningLevels: ["off"], contextWindow: 1048576 },
  { id: "qwen3.7-max", name: "Qwen3.7 Max", providerId: "alibaba", tier: "strong", reasoningLevels: ["off"], contextWindow: 1048576 },
  // DashScope Kimi, first-party ids (probed 2026-10-26: kimi-k3 tiers track
  // upstream, k2.5/k2.6 default to thinking off, K2.7 Code always thinks).
  { id: "kimi-k3", name: "Kimi K3", providerId: "alibaba", tier: "strong", reasoningLevels: KIMI_K3_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "kimi-k2.7-code", name: "Kimi K2.7 Code", providerId: "alibaba", tier: "strong", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-k2.6", name: "Kimi K2.6", providerId: "alibaba", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  { id: "kimi-k2.5", name: "Kimi K2.5", providerId: "alibaba", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  // Moonshot passthrough ids, per DashScope's Kimi doc: K3 is max-only, K2.7
  // Code always thinks, K2.6 toggles via enable_thinking.
  { id: "kimi/kimi-k3.1-pro", name: "Kimi K3.1 Pro", providerId: "alibaba", tier: "strong", reasoningLevels: KIMI_PASSTHROUGH_K3_LEVELS, defaultReasoningLevel: "max", contextWindow: 1048576 },
  { id: "kimi/kimi-k3.1-pro-highspeed", name: "Kimi K3.1 Pro Highspeed", providerId: "alibaba", tier: "fast", reasoningLevels: KIMI_PASSTHROUGH_K3_LEVELS, defaultReasoningLevel: "max", contextWindow: 1048576 },
  { id: "kimi/kimi-k3", name: "Kimi K3 (Moonshot)", providerId: "alibaba", tier: "strong", reasoningLevels: KIMI_PASSTHROUGH_K3_LEVELS, defaultReasoningLevel: "max", contextWindow: 1048576 },
  { id: "kimi/kimi-k2.7-code", name: "Kimi K2.7 Code (Moonshot)", providerId: "alibaba", tier: "strong", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi/kimi-k2.7-code-highspeed", name: "Kimi K2.7 Code Highspeed", providerId: "alibaba", tier: "fast", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi/kimi-k2.6", name: "Kimi K2.6 (Moonshot)", providerId: "alibaba", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  // Bailian token plan (GET /models on the plan endpoint, probed 2026-08-04).
  // Image/TTS entries from that list are omitted — this is a text agent.
  { id: "qwen3.8-max", name: "Qwen3.8 Max", providerId: "bailian-token-plan", tier: "strong", reasoningLevels: BAILIAN_FULL_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "qwen3.8-max-preview", name: "Qwen3.8 Max Preview", providerId: "bailian-token-plan", tier: "strong", reasoningLevels: BAILIAN_FULL_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "qwen3.7-max", name: "Qwen3.7 Max", providerId: "bailian-token-plan", tier: "strong", reasoningLevels: BAILIAN_NO_MAX_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "qwen3.7-plus", name: "Qwen3.7 Plus", providerId: "bailian-token-plan", tier: "balanced", reasoningLevels: BAILIAN_NO_MAX_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "qwen3.6-flash", name: "Qwen3.6 Flash", providerId: "bailian-token-plan", tier: "fast", reasoningLevels: BAILIAN_NO_MAX_LEVELS, defaultReasoningLevel: "medium", contextWindow: 1048576 },
  { id: "glm-5.2", name: "GLM-5.2", providerId: "bailian-token-plan", tier: "strong", reasoningLevels: BAILIAN_FULL_LEVELS, defaultReasoningLevel: "high", contextWindow: 1000000 },
  { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro", providerId: "bailian-token-plan", tier: "strong", reasoningLevels: BAILIAN_DEEPSEEK_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "deepseek-v4-flash-0731", name: "DeepSeek V4 Flash 0731", providerId: "bailian-token-plan", tier: "fast", reasoningLevels: BAILIAN_DEEPSEEK_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "doubao-seed-2-1-pro-260628", name: "Doubao Seed 2.1 Pro", providerId: "doubao", reasoningLevels: DOUBAO_SEED_REASONING_LEVELS, defaultReasoningLevel: "high" },
  { id: "MiniMax-M3", name: "MiniMax M3", providerId: "minimax", tier: "strong", reasoningLevels: MINIMAX_M3_REASONING_LEVELS, contextWindow: 1000000 },
  { id: "MiniMax-M2.7", name: "MiniMax M2.7", providerId: "minimax", tier: "balanced", reasoningLevels: MINIMAX_REASONING_LEVELS, contextWindow: 204800 },
  { id: "MiniMax-M2.7-highspeed", name: "MiniMax M2.7 Highspeed", providerId: "minimax", tier: "fast", reasoningLevels: MINIMAX_REASONING_LEVELS, contextWindow: 204800 },
  { id: "MiniMax-M3", name: "MiniMax M3", providerId: "minimax-anthropic", tier: "strong", reasoningLevels: MINIMAX_M3_REASONING_LEVELS, contextWindow: 1000000 },
  { id: "MiniMax-M2.7", name: "MiniMax M2.7", providerId: "minimax-anthropic", tier: "balanced", reasoningLevels: MINIMAX_REASONING_LEVELS, contextWindow: 204800 },
  { id: "MiniMax-M2.7-highspeed", name: "MiniMax M2.7 Highspeed", providerId: "minimax-anthropic", tier: "fast", reasoningLevels: MINIMAX_REASONING_LEVELS, contextWindow: 204800 },
  // Token Plan's text models; V2.5 remains available until 2026-10-21.
  // https://mimo.mi.com/docs/zh-CN/tokenplan/integration/claudecode
  { id: "mimo-v2.6-pro", name: "MiMo V2.6 Pro", providerId: "mimo-token-plan", tier: "strong", reasoningLevels: TOGGLE_THINKING_LEVELS, defaultReasoningLevel: "medium", contextWindow: 1000000 },
  { id: "mimo-v2.6-flash", name: "MiMo V2.6 Flash", providerId: "mimo-token-plan", tier: "fast", reasoningLevels: TOGGLE_THINKING_LEVELS, defaultReasoningLevel: "medium", contextWindow: 1000000 },
  { id: "mimo-v2.5-pro", name: "MiMo V2.5 Pro", providerId: "mimo-token-plan", reasoningLevels: TOGGLE_THINKING_LEVELS, defaultReasoningLevel: "medium", contextWindow: 1000000 },
  { id: "mimo-v2.5", name: "MiMo V2.5", providerId: "mimo-token-plan", reasoningLevels: TOGGLE_THINKING_LEVELS, defaultReasoningLevel: "medium", contextWindow: 1000000 },
  { id: "step-3.7-flash", name: "Step 3.7 Flash", providerId: "stepfun", reasoningLevels: STEPFUN_REASONING_LEVELS, contextWindow: 256000 },
  { id: "step-3.5-flash-2603", name: "Step 3.5 Flash 2603", providerId: "stepfun", reasoningLevels: STEPFUN_REASONING_LEVELS },
  { id: "step-3.5-flash", name: "Step 3.5 Flash", providerId: "stepfun", reasoningLevels: STEPFUN_REASONING_LEVELS },
  { id: "step-router-v1", name: "Step Router V1", providerId: "stepfun", reasoningLevels: STEPFUN_REASONING_LEVELS },
  // Use server-default reasoning until this model's effort controls are documented.
  { id: "water18-0910", name: "water18-0910", providerId: "stepfun-api", reasoningLevels: [] },
  { id: "kimi-k2.7-code", name: "Kimi K2.7 Code", providerId: "moonshot-cn", tier: "strong", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-k2.7-code-highspeed", name: "Kimi K2.7 Code Highspeed", providerId: "moonshot-cn", tier: "fast", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-k3", name: "Kimi K3", providerId: "moonshot-cn", tier: "strong", reasoningLevels: KIMI_K3_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "kimi-k3.1-pro", name: "Kimi K3.1 Pro", providerId: "moonshot-cn", tier: "strong", reasoningLevels: KIMI_K3_EFFORT_ONLY_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "kimi-k3.1-pro-highspeed", name: "Kimi K3.1 Pro Highspeed", providerId: "moonshot-cn", tier: "fast", reasoningLevels: KIMI_K3_EFFORT_ONLY_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "kimi-k2.6", name: "Kimi K2.6", providerId: "moonshot-cn", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  { id: "kimi-k2.5", name: "Kimi K2.5", providerId: "moonshot-cn", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  { id: "kimi-k2.7-code", name: "Kimi K2.7 Code", providerId: "moonshot-intl", tier: "strong", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-k2.7-code-highspeed", name: "Kimi K2.7 Code Highspeed", providerId: "moonshot-intl", tier: "fast", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-k3", name: "Kimi K3", providerId: "moonshot-intl", tier: "strong", reasoningLevels: KIMI_K3_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "kimi-k3.1-pro", name: "Kimi K3.1 Pro", providerId: "moonshot-intl", tier: "strong", reasoningLevels: KIMI_K3_EFFORT_ONLY_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "kimi-k3.1-pro-highspeed", name: "Kimi K3.1 Pro Highspeed", providerId: "moonshot-intl", tier: "fast", reasoningLevels: KIMI_K3_EFFORT_ONLY_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "kimi-k2.6", name: "Kimi K2.6", providerId: "moonshot-intl", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  { id: "kimi-k2.5", name: "Kimi K2.5", providerId: "moonshot-intl", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  // Kimi for Coding serves plan-slot ids (verified via GET /coding/v1/models):
  // k3, k3-256k, k3.1-pro, k3.1-pro-highspeed, kimi-for-coding,
  // kimi-for-coding-highspeed.
  { id: "k3", name: "Kimi K3", providerId: "kimi-for-coding", tier: "strong", reasoningLevels: KIMI_K3_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "k3-256k", name: "Kimi K3 256K", providerId: "kimi-for-coding", tier: "strong", reasoningLevels: KIMI_K3_EFFORT_ONLY_LEVELS, defaultReasoningLevel: "high", contextWindow: 262144 },
  { id: "k3.1-pro", name: "Kimi K3.1 Pro", providerId: "kimi-for-coding", tier: "strong", reasoningLevels: KIMI_K3_EFFORT_ONLY_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "k3.1-pro-highspeed", name: "Kimi K3.1 Pro Highspeed", providerId: "kimi-for-coding", tier: "fast", reasoningLevels: KIMI_K3_EFFORT_ONLY_LEVELS, defaultReasoningLevel: "high", contextWindow: 1048576 },
  { id: "kimi-for-coding", name: "Kimi for Coding", providerId: "kimi-for-coding", tier: "strong", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-for-coding-highspeed", name: "Kimi for Coding Highspeed", providerId: "kimi-for-coding", tier: "fast", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-k2.7-code", name: "Kimi K2.7 Code", providerId: "kimi-for-coding", tier: "strong", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-k2.7-code-highspeed", name: "Kimi K2.7 Code Highspeed", providerId: "kimi-for-coding", tier: "fast", reasoningLevels: KIMI_THINKING_ONLY_LEVELS, contextWindow: 262144 },
  { id: "kimi-k2.6", name: "Kimi K2.6", providerId: "kimi-for-coding", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  { id: "kimi-k2.5", name: "Kimi K2.5", providerId: "kimi-for-coding", reasoningLevels: TOGGLE_THINKING_LEVELS, contextWindow: 256000 },
  { id: "llama-3.3-70b-versatile", name: "llama-3.3-70b-versatile", providerId: "groq", reasoningLevels: ["off"], contextWindow: 131072 },
  { id: "mixtral-8x7b-32768", name: "mixtral-8x7b-32768", providerId: "groq", reasoningLevels: ["off"], contextWindow: 32768 },
  { id: "gemma-2-9b-it", name: "gemma-2-9b-it", providerId: "groq", reasoningLevels: ["off"], contextWindow: 32768 },
  { id: "meta-llama/Llama-3.3-70B-Instruct-Turbo", name: "meta-llama/Llama-3.3-70B-Instruct-Turbo", providerId: "together", reasoningLevels: ["off"], contextWindow: 131072 },
  { id: "Qwen/Qwen2.5-72B-Instruct", name: "Qwen/Qwen2.5-72B-Instruct", providerId: "together", reasoningLevels: ["off"], contextWindow: 32768 },
  { id: "accounts/fireworks/models/kimi-k2p6", name: "Kimi-K2.6", providerId: "fireworks", reasoningLevels: ["off"], contextWindow: 256000 },
  { id: "llama3.1", name: "llama3.1", providerId: "local", reasoningLevels: ["off"], contextWindow: 32768 },
  { id: "qwen2.5", name: "qwen2.5", providerId: "local", reasoningLevels: ["off"], contextWindow: 32768 },
  { id: "deepseek-coder-v2", name: "deepseek-coder-v2", providerId: "local", reasoningLevels: ["off"], contextWindow: 32768 },
];

export function listBuiltinModels(providerId: string): BuiltinModelDefinition[] {
  return BUILTIN_MODELS.filter((model) => model.providerId === providerId);
}

// Runtime overlay populated from provider-side discovery (e.g. ChatGPT codex /models).
// Looked up before the static catalog so newly-released models work without a code change.
const dynamicOverlay = new Map<string, BuiltinModelDefinition>();

function overlayKey(providerId: string, modelId: string): string {
  return `${providerId}:${modelId}`;
}

export function registerDynamicModelMetadata(model: BuiltinModelDefinition): void {
  dynamicOverlay.set(overlayKey(model.providerId, model.id), model);
}

export function clearDynamicModelMetadata(providerId: string): void {
  const providerIds = providerId === "openai"
    ? ["openai", "openai-codex"]
    : [providerId];
  for (const key of dynamicOverlay.keys()) {
    if (providerIds.some((id) => key.startsWith(`${id}:`))) dynamicOverlay.delete(key);
  }
}

export function replaceDynamicModelMetadata(
  providerId: string,
  models: readonly BuiltinModelDefinition[],
): void {
  clearDynamicModelMetadata(providerId);
  const acceptedProviderIds = providerId === "openai"
    ? new Set(["openai", "openai-codex"])
    : new Set([providerId]);
  for (const model of models) {
    if (acceptedProviderIds.has(model.providerId)) registerDynamicModelMetadata(model);
  }
}

/**
 * All dynamic-overlay entries for a provider (openai includes its
 * openai-codex catalog alias). The overlay was previously visible only via
 * getBuiltinModel point lookups; the routing snapshot builder needs the list.
 */
export function listDynamicModelMetadata(providerId: string): BuiltinModelDefinition[] {
  const providerIds = providerId === "openai" ? ["openai", "openai-codex"] : [providerId];
  const out: BuiltinModelDefinition[] = [];
  for (const [key, model] of dynamicOverlay) {
    if (providerIds.some((id) => key.startsWith(`${id}:`))) out.push(model);
  }
  return out;
}

export function getBuiltinModel(providerId: string, modelId: string): BuiltinModelDefinition | undefined {
  const overlayHit = dynamicOverlay.get(overlayKey(providerId, modelId))
    || (providerId === "openai" ? dynamicOverlay.get(overlayKey("openai-codex", modelId)) : undefined);
  if (overlayHit) return overlayHit;
  const staticHit = BUILTIN_MODELS.find((model) => model.providerId === providerId && model.id === modelId)
    || (providerId === "openai"
      ? BUILTIN_MODELS.find((model) => model.providerId === "openai-codex" && model.id === modelId)
      : undefined);
  if (staticHit) return staticHit;
  // Dynamically-discovered grok models aren't in the static catalog until
  // remote discovery runs. Synthesize their metadata from the id so reasoning
  // levels, default effort, and context window resolve correctly on a fresh
  // start or resumed session (before the first /model discovery).
  if (providerId === "grok") {
    const inferred = inferGrokModelMetadata(modelId);
    return {
      id: modelId,
      name: modelId,
      providerId: "grok",
      reasoningLevels: inferred.levels,
      defaultReasoningLevel: inferred.defaultLevel,
      contextWindow: inferred.contextWindow,
    };
  }
  return undefined;
}

export function getModelDefaultReasoningLevel(providerId: string, modelId: string): ReasoningEffort | undefined {
  return getBuiltinModel(providerId, modelId)?.defaultReasoningLevel;
}

export function getBuiltinProvider(providerId: string): BuiltinProviderDefinition | undefined {
  return BUILTIN_PROVIDERS.find((provider) => provider.id === providerId);
}

/**
 * Infer capability metadata for a Grok model id the /models endpoint returned
 * without metadata. Flagship reasoning models (grok-N.M) expose low/medium/high
 * with thinking always on and a 500K context window — matching the curated
 * grok-4.5 entry — while composer/fast/mini/flash variants have no effort
 * control and a 200K window. Pure (id-only) so it works even before remote
 * model discovery runs.
 */
export function inferGrokModelMetadata(modelId: string): { levels: ReasoningEffort[]; defaultLevel?: ReasoningEffort; contextWindow?: number } {
  const id = modelId.toLowerCase();
  if (/(composer|fast|mini|flash)/.test(id)) {
    return { levels: ["off"], contextWindow: 200000 };
  }
  return { levels: ["low", "medium", "high"], defaultLevel: "high", contextWindow: 500000 };
}

export function getModelContextWindow(providerId: string, modelId: string): number | undefined {
  const builtin = getBuiltinModel(providerId, modelId);
  if (builtin?.contextWindow !== undefined) return builtin.contextWindow;
  // Dynamically-discovered grok models aren't in the static catalog until
  // discovery runs; infer the window from the id so context usage can render a
  // percentage even before discovery (e.g. on a resumed session right after
  // startup).
  if (providerId === "grok") return inferGrokModelMetadata(modelId).contextWindow;
  return undefined;
}

export function getToolOutputTokenLimit(providerId: string, modelId: string): number | undefined {
  return getBuiltinModel(providerId, modelId)?.toolOutputTokenLimit;
}
