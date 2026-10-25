// 本地模型：主流 OpenAI 兼容厂商预设（Base URL / 模型名 / 上下文）。
// 选型以「可直连 chat/completions」为准；列表会随厂商更新，手工「自定义」可兜底。
// 上下文为厂商公开的最大窗口（个别以保守值计）；模型价格/限额以厂商控制台为准。
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
        { id: "gpt-5.1", label: "GPT-5.1", context: 400000 },
        { id: "gpt-5", label: "GPT-5", context: 400000 },
        { id: "gpt-5-mini", label: "GPT-5 mini", context: 400000 },
        { id: "gpt-5-nano", label: "GPT-5 nano", context: 400000 },
        { id: "gpt-4.1", label: "GPT-4.1", context: 1047576 },
        { id: "gpt-4.1-mini", label: "GPT-4.1 mini", context: 1047576 },
        { id: "gpt-4o", label: "GPT-4o", context: 128000 },
        { id: "o3", label: "o3（推理）", context: 200000 },
        { id: "o4-mini", label: "o4-mini（推理）", context: 200000 },
      ],
    },
    {
      id: "anthropic",
      label: "Anthropic（Claude）",
      provider: "anthropic",
      baseUrl: "https://api.anthropic.com/v1",
      keyHint: "sk-ant-…",
      keyHelp: "console.anthropic.com → API Keys（OpenAI 兼容端点）",
      models: [
        { id: "claude-opus-5-5", label: "Claude Opus 5.5", context: 200000 },
        { id: "claude-sonnet-5-5", label: "Claude Sonnet 5.5", context: 200000 },
        { id: "claude-haiku-5-5", label: "Claude Haiku 5.5", context: 200000 },
        { id: "claude-fable-5-1", label: "Claude Fable 5.1", context: 200000 },
        { id: "claude-sonnet-4-5", label: "Claude Sonnet 4.5", context: 200000 },
      ],
    },
    {
      id: "google",
      label: "Google（Gemini）",
      provider: "google",
      baseUrl: "https://generativelanguage.googleapis.com/v1beta/openai",
      keyHint: "AIza…",
      keyHelp: "aistudio.google.com → Get API key（OpenAI 兼容端点）",
      models: [
        { id: "gemini-3-pro-preview", label: "Gemini 3 Pro（预览）", context: 1048576 },
        { id: "gemini-2.5-pro", label: "Gemini 2.5 Pro", context: 1048576 },
        { id: "gemini-2.5-flash", label: "Gemini 2.5 Flash", context: 1048576 },
        { id: "gemini-2.5-flash-lite", label: "Gemini 2.5 Flash-Lite", context: 1048576 },
        { id: "gemini-2.0-flash", label: "Gemini 2.0 Flash", context: 1048576 },
      ],
    },
    {
      id: "xai",
      label: "xAI（Grok）",
      provider: "xai",
      baseUrl: "https://api.x.ai/v1",
      keyHint: "xai-…",
      keyHelp: "console.x.ai → API Keys",
      models: [
        { id: "grok-4", label: "Grok 4", context: 256000 },
        { id: "grok-4-fast", label: "Grok 4 Fast", context: 2000000 },
        { id: "grok-code-fast-1", label: "Grok Code Fast", context: 256000 },
        { id: "grok-3", label: "Grok 3", context: 131072 },
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
        { id: "qwen3-max", label: "Qwen3-Max", context: 262144 },
        { id: "qwen-plus", label: "Qwen-Plus", context: 131072 },
        { id: "qwen-max", label: "Qwen-Max", context: 32768 },
        { id: "qwen-turbo", label: "Qwen-Turbo", context: 131072 },
        { id: "qwen-flash", label: "Qwen-Flash", context: 1000000 },
        { id: "qwen-long", label: "Qwen-Long", context: 10000000 },
        { id: "qwen3-coder-plus", label: "Qwen3-Coder-Plus", context: 262144 },
        { id: "qwen3-vl-plus", label: "Qwen3-VL-Plus（视觉）", context: 262144 },
        { id: "qwen-vl-max", label: "Qwen-VL-Max（视觉）", context: 32768 },
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
        { id: "deepseek-chat", label: "DeepSeek-Chat（V3.2）", context: 131072 },
        { id: "deepseek-reasoner", label: "DeepSeek-Reasoner（V3.2 思考）", context: 131072 },
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
        { id: "kimi-k2-thinking", label: "Kimi K2 Thinking（思考）", context: 262144 },
        { id: "kimi-k2-0905-preview", label: "Kimi K2", context: 262144 },
        { id: "kimi-k2-turbo-preview", label: "Kimi K2 Turbo", context: 262144 },
        { id: "kimi-latest", label: "kimi-latest", context: 131072 },
        { id: "moonshot-v1-128k", label: "moonshot-v1-128k", context: 131072 },
        { id: "moonshot-v1-32k", label: "moonshot-v1-32k", context: 32768 },
        { id: "moonshot-v1-8k", label: "moonshot-v1-8k", context: 8192 },
      ],
    },
    {
      id: "zhipu",
      label: "智谱 GLM",
      provider: "zhipu",
      baseUrl: "https://open.bigmodel.cn/api/paas/v4",
      keyHint: "id.secret",
      keyHelp: "open.bigmodel.cn API Key（形如 id.secret）",
      models: [
        { id: "glm-4.6", label: "GLM-4.6", context: 204800 },
        { id: "glm-4.5", label: "GLM-4.5", context: 131072 },
        { id: "glm-4.5-air", label: "GLM-4.5-Air", context: 131072 },
        { id: "glm-4.5-flash", label: "GLM-4.5-Flash（免费）", context: 131072 },
        { id: "glm-4-plus", label: "GLM-4-Plus", context: 128000 },
        { id: "glm-z1-air", label: "GLM-Z1-Air（推理）", context: 131072 },
      ],
    },
    {
      id: "volcengine",
      label: "火山方舟（豆包）",
      provider: "volcengine",
      baseUrl: "https://ark.cn-beijing.volces.com/api/v3",
      keyHint: "…",
      keyHelp: "console.volcengine.com → 方舟 → API Key（模型名带版本日期）",
      models: [
        { id: "doubao-seed-1-6-250615", label: "Doubao-Seed-1.6", context: 262144 },
        { id: "doubao-seed-1-6-thinking-250715", label: "Doubao-Seed-1.6-Thinking（思考）", context: 262144 },
        { id: "doubao-seed-1-6-flash-250715", label: "Doubao-Seed-1.6-Flash", context: 262144 },
        { id: "doubao-1-5-pro-32k-250115", label: "Doubao-1.5-Pro-32k", context: 32768 },
      ],
    },
    {
      id: "hunyuan",
      label: "腾讯混元",
      provider: "hunyuan",
      baseUrl: "https://api.hunyuan.cloud.tencent.com/v1",
      keyHint: "sk-…",
      keyHelp: "console.cloud.tencent.com → 混元 → API Key",
      models: [
        { id: "hunyuan-turbos-latest", label: "Hunyuan-TurboS", context: 32768 },
        { id: "hunyuan-t1-latest", label: "Hunyuan-T1（推理）", context: 65536 },
        { id: "hunyuan-lite", label: "Hunyuan-Lite（免费）", context: 262144 },
      ],
    },
    {
      id: "qianfan",
      label: "百度千帆（文心）",
      provider: "qianfan",
      baseUrl: "https://qianfan.baidubce.com/v2",
      keyHint: "bce-v3/…",
      keyHelp: "console.bce.baidu.com → 千帆 → API Key（v2 OpenAI 兼容）",
      models: [
        { id: "ernie-4.5-turbo-128k", label: "ERNIE 4.5 Turbo 128k", context: 131072 },
        { id: "ernie-4.5-turbo-vl-32k", label: "ERNIE 4.5 Turbo VL（视觉）", context: 32768 },
        { id: "ernie-x1-turbo-32k", label: "ERNIE X1 Turbo（推理）", context: 32768 },
        { id: "ernie-speed-128k", label: "ERNIE Speed 128k（免费）", context: 131072 },
      ],
    },
    {
      id: "spark",
      label: "讯飞星火",
      provider: "spark",
      baseUrl: "https://spark-api-open.xf-yun.com/v1",
      keyHint: "APIPassword",
      keyHelp: "console.xfyun.cn → http 服务接口认证（APIPassword）",
      models: [
        { id: "x1", label: "Spark X1（推理）", context: 32768 },
        { id: "4.0Ultra", label: "Spark 4.0 Ultra", context: 8192 },
        { id: "max-32k", label: "Spark Max-32k", context: 32768 },
        { id: "pro-128k", label: "Spark Pro-128k", context: 131072 },
        { id: "lite", label: "Spark Lite（免费）", context: 8192 },
      ],
    },
    {
      id: "minimax",
      label: "MiniMax",
      provider: "minimax",
      baseUrl: "https://api.minimaxi.com/v1",
      keyHint: "…",
      keyHelp: "platform.minimaxi.com（国内：api.minimax.chat）",
      models: [
        { id: "MiniMax-M2", label: "MiniMax-M2", context: 204800 },
        { id: "MiniMax-M1", label: "MiniMax-M1（推理）", context: 1000000 },
        { id: "MiniMax-Text-01", label: "MiniMax-Text-01", context: 1000000 },
      ],
    },
    {
      id: "stepfun",
      label: "阶跃星辰（Step）",
      provider: "stepfun",
      baseUrl: "https://api.stepfun.com/v1",
      keyHint: "…",
      keyHelp: "platform.stepfun.com",
      models: [
        { id: "step-3", label: "Step-3", context: 65536 },
        { id: "step-2-16k", label: "Step-2-16k", context: 16384 },
        { id: "step-2-mini", label: "Step-2-mini", context: 8192 },
      ],
    },
    {
      id: "mistral",
      label: "Mistral",
      provider: "mistral",
      baseUrl: "https://api.mistral.ai/v1",
      keyHint: "…",
      keyHelp: "console.mistral.ai → API Keys",
      models: [
        { id: "mistral-large-latest", label: "Mistral Large", context: 131072 },
        { id: "mistral-medium-latest", label: "Mistral Medium", context: 131072 },
        { id: "magistral-medium-latest", label: "Magistral（推理）", context: 40960 },
        { id: "codestral-latest", label: "Codestral（代码）", context: 262144 },
        { id: "mistral-small-latest", label: "Mistral Small", context: 131072 },
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
        { id: "deepseek-ai/DeepSeek-V3.1", label: "DeepSeek-V3.1", context: 131072 },
        { id: "deepseek-ai/DeepSeek-R1", label: "DeepSeek-R1", context: 131072 },
        { id: "zai-org/GLM-4.6", label: "GLM-4.6", context: 204800 },
        { id: "Qwen/Qwen3-235B-A22B-Instruct-2507", label: "Qwen3-235B-A22B", context: 262144 },
        { id: "moonshotai/Kimi-K2-Instruct-0905", label: "Kimi-K2-Instruct", context: 262144 },
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
        { id: "openai/gpt-oss-20b", label: "GPT-OSS 20B", context: 131072 },
        { id: "moonshotai/kimi-k2-instruct-0905", label: "Kimi K2 Instruct", context: 262144 },
        { id: "qwen/qwen3-32b", label: "Qwen3 32B", context: 131072 },
      ],
    },
    {
      id: "openrouter",
      label: "OpenRouter",
      provider: "openrouter",
      baseUrl: "https://openrouter.ai/api/v1",
      keyHint: "sk-or-…",
      keyHelp: "openrouter.ai/keys（聚合，模型名=厂商/模型）",
      models: [
        { id: "openai/gpt-5.1", label: "OpenAI GPT-5.1", context: 400000 },
        { id: "openai/gpt-4o", label: "OpenAI GPT-4o", context: 128000 },
        { id: "anthropic/claude-sonnet-4.5", label: "Claude Sonnet 4.5", context: 200000 },
        { id: "google/gemini-3-pro-preview", label: "Gemini 3 Pro（预览）", context: 1048576 },
        { id: "google/gemini-2.5-flash", label: "Gemini 2.5 Flash", context: 1048576 },
        { id: "x-ai/grok-4", label: "Grok 4", context: 256000 },
        { id: "deepseek/deepseek-chat-v3.1", label: "DeepSeek Chat V3.1", context: 131072 },
        { id: "qwen/qwen3-235b-a22b", label: "Qwen3 235B", context: 131072 },
        { id: "moonshotai/kimi-k2-0905", label: "Kimi K2", context: 262144 },
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
        { id: "meta-llama/Llama-4-Maverick-17B-128E-Instruct-FP8", label: "Llama 4 Maverick", context: 524288 },
        { id: "meta-llama/Llama-4-Scout-17B-16E-Instruct", label: "Llama 4 Scout", context: 327680 },
        { id: "meta-llama/Llama-3.3-70B-Instruct-Turbo", label: "Llama 3.3 70B Turbo", context: 131072 },
        { id: "deepseek-ai/DeepSeek-V3", label: "DeepSeek-V3", context: 131072 },
        { id: "Qwen/Qwen2.5-72B-Instruct-Turbo", label: "Qwen2.5 72B Turbo", context: 32768 },
      ],
    },
    {
      id: "perplexity",
      label: "Perplexity（联网搜索）",
      provider: "perplexity",
      baseUrl: "https://api.perplexity.ai",
      keyHint: "pplx-…",
      keyHelp: "perplexity.ai/settings/api",
      models: [
        { id: "sonar-pro", label: "Sonar Pro", context: 200000 },
        { id: "sonar", label: "Sonar", context: 128000 },
        { id: "sonar-reasoning-pro", label: "Sonar Reasoning Pro（推理）", context: 128000 },
      ],
    },
    {
      id: "ollama",
      label: "Ollama（本机）",
      provider: "ollama",
      baseUrl: "http://127.0.0.1:11434/v1",
      keyHint: "ollama（可任意）",
      keyHelp: "本机 Ollama；Key 可填 ollama；模型名=ollama list 所示",
      models: [
        { id: "gpt-oss", label: "gpt-oss", context: 131072 },
        { id: "qwen3", label: "qwen3", context: 40960 },
        { id: "llama3.3", label: "llama3.3", context: 131072 },
        { id: "llama3.2", label: "llama3.2", context: 128000 },
        { id: "deepseek-r1", label: "deepseek-r1", context: 131072 },
        { id: "gemma3", label: "gemma3", context: 131072 },
        { id: "qwen2.5-coder", label: "qwen2.5-coder", context: 32768 },
        { id: "mistral", label: "mistral", context: 32768 },
      ],
    },
    {
      id: "lmstudio",
      label: "LM Studio（本机）",
      provider: "lmstudio",
      baseUrl: "http://127.0.0.1:1234/v1",
      keyHint: "lm-studio（可任意）",
      keyHelp: "本机 LM Studio 开启本地服务器；模型名见其「已加载模型」",
      models: [],
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
