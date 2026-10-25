// 本地模型：主流 OpenAI 兼容厂商预设（Base URL / 模型名 / 上下文）。
// 选型以「可直连 chat/completions」为准；列表会随厂商更新，手工「自定义」可兜底。
(function () {
  /** @typedef {{ id: string, label: string, context?: number }} LlmModelPreset */
  /** @typedef {{ id: string, label: string, provider: string, baseUrl: string, keyHint?: string, keyHelp?: string, models: LlmModelPreset[] }} LlmVendorPreset */

  /** @type {LlmVendorPreset[]} */
  const VENDORS = [
    {
      id: "openai",
      label: "OpenAI",
      provider: "openai",
      baseUrl: "https://api.openai.com/v1",
      keyHint: "sk-…",
      keyHelp: "platform.openai.com → API keys",
      models: [
        { id: "gpt-4o", label: "GPT-4o", context: 128000 },
        { id: "gpt-4o-mini", label: "GPT-4o mini", context: 128000 },
        { id: "gpt-4.1", label: "GPT-4.1", context: 1047576 },
        { id: "gpt-4.1-mini", label: "GPT-4.1 mini", context: 1047576 },
        { id: "o4-mini", label: "o4-mini", context: 200000 },
        { id: "o3-mini", label: "o3-mini", context: 200000 },
      ],
    },
    {
      id: "dashscope",
      label: "阿里云百炼（Qwen）",
      provider: "dashscope",
      baseUrl: "https://dashscope.aliyuncs.com/compatible-mode/v1",
      keyHint: "sk-…",
      keyHelp: "百炼控制台 API Key（北京区兼容模式）",
      models: [
        { id: "qwen-plus", label: "Qwen-Plus", context: 131072 },
        { id: "qwen-max", label: "Qwen-Max", context: 32768 },
        { id: "qwen-turbo", label: "Qwen-Turbo", context: 131072 },
        { id: "qwen-flash", label: "Qwen-Flash", context: 1000000 },
        { id: "qwen-long", label: "Qwen-Long", context: 10000000 },
        { id: "qwen3-max", label: "Qwen3-Max", context: 262144 },
        { id: "qwen3-coder-plus", label: "Qwen3-Coder-Plus", context: 262144 },
        { id: "qwen-vl-max", label: "Qwen-VL-Max", context: 32768 },
        { id: "qwen-vl-plus", label: "Qwen-VL-Plus", context: 32768 },
      ],
    },
    {
      id: "deepseek",
      label: "DeepSeek",
      provider: "deepseek",
      baseUrl: "https://api.deepseek.com/v1",
      keyHint: "sk-…",
      keyHelp: "platform.deepseek.com",
      models: [
        { id: "deepseek-chat", label: "DeepSeek-Chat (V3)", context: 65536 },
        { id: "deepseek-reasoner", label: "DeepSeek-Reasoner (R1)", context: 65536 },
      ],
    },
    {
      id: "moonshot",
      label: "月之暗面（Kimi）",
      provider: "moonshot",
      baseUrl: "https://api.moonshot.cn/v1",
      keyHint: "sk-…",
      keyHelp: "platform.moonshot.cn",
      models: [
        { id: "moonshot-v1-8k", label: "moonshot-v1-8k", context: 8192 },
        { id: "moonshot-v1-32k", label: "moonshot-v1-32k", context: 32768 },
        { id: "moonshot-v1-128k", label: "moonshot-v1-128k", context: 131072 },
        { id: "kimi-k2-0905-preview", label: "Kimi K2", context: 262144 },
        { id: "kimi-latest", label: "kimi-latest", context: 131072 },
      ],
    },
    {
      id: "zhipu",
      label: "智谱 GLM",
      provider: "zhipu",
      baseUrl: "https://open.bigmodel.cn/api/paas/v4",
      keyHint: "…",
      keyHelp: "open.bigmodel.cn API Key",
      models: [
        { id: "glm-4-plus", label: "GLM-4-Plus", context: 128000 },
        { id: "glm-4-air", label: "GLM-4-Air", context: 128000 },
        { id: "glm-4-flash", label: "GLM-4-Flash", context: 128000 },
        { id: "glm-4.5", label: "GLM-4.5", context: 128000 },
        { id: "glm-4.5-air", label: "GLM-4.5-Air", context: 128000 },
        { id: "glm-z1-air", label: "GLM-Z1-Air", context: 128000 },
      ],
    },
    {
      id: "siliconflow",
      label: "硅基流动 SiliconFlow",
      provider: "siliconflow",
      baseUrl: "https://api.siliconflow.cn/v1",
      keyHint: "sk-…",
      keyHelp: "cloud.siliconflow.cn",
      models: [
        { id: "deepseek-ai/DeepSeek-V3", label: "DeepSeek-V3", context: 65536 },
        { id: "deepseek-ai/DeepSeek-R1", label: "DeepSeek-R1", context: 65536 },
        { id: "Qwen/Qwen2.5-72B-Instruct", label: "Qwen2.5-72B", context: 32768 },
        { id: "Qwen/Qwen3-235B-A22B", label: "Qwen3-235B-A22B", context: 131072 },
        { id: "moonshotai/Kimi-K2-Instruct", label: "Kimi-K2-Instruct", context: 131072 },
      ],
    },
    {
      id: "groq",
      label: "Groq",
      provider: "groq",
      baseUrl: "https://api.groq.com/openai/v1",
      keyHint: "gsk_…",
      keyHelp: "console.groq.com",
      models: [
        { id: "llama-3.3-70b-versatile", label: "Llama 3.3 70B", context: 128000 },
        { id: "llama-3.1-8b-instant", label: "Llama 3.1 8B Instant", context: 128000 },
        { id: "openai/gpt-oss-120b", label: "GPT-OSS 120B", context: 131072 },
        { id: "qwen/qwen3-32b", label: "Qwen3 32B", context: 131072 },
      ],
    },
    {
      id: "openrouter",
      label: "OpenRouter",
      provider: "openrouter",
      baseUrl: "https://openrouter.ai/api/v1",
      keyHint: "sk-or-…",
      keyHelp: "openrouter.ai/keys",
      models: [
        { id: "openai/gpt-4o", label: "OpenAI GPT-4o", context: 128000 },
        { id: "openai/gpt-4o-mini", label: "OpenAI GPT-4o mini", context: 128000 },
        { id: "anthropic/claude-sonnet-4", label: "Claude Sonnet 4", context: 200000 },
        { id: "anthropic/claude-3.5-sonnet", label: "Claude 3.5 Sonnet", context: 200000 },
        { id: "google/gemini-2.5-pro", label: "Gemini 2.5 Pro", context: 1048576 },
        { id: "deepseek/deepseek-chat", label: "DeepSeek Chat", context: 65536 },
        { id: "qwen/qwen3-235b-a22b", label: "Qwen3 235B", context: 131072 },
      ],
    },
    {
      id: "together",
      label: "Together AI",
      provider: "together",
      baseUrl: "https://api.together.xyz/v1",
      keyHint: "…",
      keyHelp: "api.together.xyz",
      models: [
        { id: "meta-llama/Meta-Llama-3.1-70B-Instruct-Turbo", label: "Llama 3.1 70B Turbo", context: 131072 },
        { id: "meta-llama/Llama-3.3-70B-Instruct-Turbo", label: "Llama 3.3 70B Turbo", context: 131072 },
        { id: "Qwen/Qwen2.5-72B-Instruct-Turbo", label: "Qwen2.5 72B Turbo", context: 32768 },
        { id: "deepseek-ai/DeepSeek-V3", label: "DeepSeek-V3", context: 65536 },
      ],
    },
    {
      id: "minimax",
      label: "MiniMax",
      provider: "minimax",
      baseUrl: "https://api.minimaxi.com/v1",
      keyHint: "…",
      keyHelp: "platform.minimaxi.com",
      models: [
        { id: "MiniMax-M2", label: "MiniMax-M2", context: 204800 },
        { id: "MiniMax-Text-01", label: "MiniMax-Text-01", context: 1000000 },
      ],
    },
    {
      id: "ollama",
      label: "Ollama（本机）",
      provider: "ollama",
      baseUrl: "http://127.0.0.1:11434/v1",
      keyHint: "ollama（可任意）",
      keyHelp: "本机 Ollama；Key 可填 ollama",
      models: [
        { id: "llama3.2", label: "llama3.2", context: 128000 },
        { id: "llama3.1", label: "llama3.1", context: 128000 },
        { id: "qwen2.5", label: "qwen2.5", context: 32768 },
        { id: "qwen2.5-coder", label: "qwen2.5-coder", context: 32768 },
        { id: "deepseek-r1", label: "deepseek-r1", context: 65536 },
        { id: "mistral", label: "mistral", context: 32768 },
      ],
    },
    {
      id: "custom",
      label: "自定义（OpenAI 兼容）",
      provider: "custom",
      baseUrl: "",
      keyHint: "sk-…",
      keyHelp: "任意 OpenAI 兼容端点",
      models: [],
    },
  ];

  function list() {
    return VENDORS.slice();
  }

  function get(id) {
    return VENDORS.find((v) => v.id === id) || null;
  }

  /** 编辑回填：按 baseUrl / provider / model 猜测厂商。 */
  function match(provider, baseUrl, model) {
    const p = (provider || "").trim().toLowerCase();
    const b = (baseUrl || "").trim().replace(/\/+$/, "").toLowerCase();
    const m = (model || "").trim();

    for (const v of VENDORS) {
      if (v.id === "custom") continue;
      const vb = (v.baseUrl || "").replace(/\/+$/, "").toLowerCase();
      if (b && vb && (b === vb || b.startsWith(vb) || vb.startsWith(b))) return v;
    }
    for (const v of VENDORS) {
      if (v.id === "custom") continue;
      if (p && (v.id === p || v.provider === p)) return v;
    }
    if (m) {
      for (const v of VENDORS) {
        if (v.models.some((x) => x.id === m)) return v;
      }
    }
    return get("custom");
  }

  function modelInVendor(vendor, modelId) {
    if (!vendor || !modelId) return null;
    return vendor.models.find((x) => x.id === modelId) || null;
  }

  window.LlmPresets = { list, get, match, modelInVendor };
})();
