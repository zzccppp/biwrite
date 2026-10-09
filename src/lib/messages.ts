// UI strings in English and Chinese. Keys are grouped by area. `{name}`
// marks a value filled in by `t()`.

export const messages = {
  // ── Common ─────────────────────────────────────────────────────────
  "common.close": { en: "Close", zh: "关闭" },
  "common.cancel": { en: "Cancel", zh: "取消" },
  "common.save": { en: "Save", zh: "保存" },
  "common.saved": { en: "Saved.", zh: "已保存。" },
  "common.remove": { en: "Remove", zh: "删除" },
  "common.test": { en: "Test", zh: "测试" },
  "common.testing": { en: "Testing…", zh: "测试中…" },
  "common.default": { en: "Default", zh: "默认" },

  // ── Settings: language and translation pacing ─────────────────────
  "settings.language": { en: "Language", zh: "界面语言" },
  "settings.languageHint": {
    en: "The interface language. Translation directions do not change.",
    zh: "只改变界面文字，翻译方向不变。",
  },
  "settings.pacing": { en: "Requests", zh: "请求节奏" },
  "settings.batch": { en: "Paragraphs per request · {n}", zh: "每次请求的段落数 · {n}" },
  "settings.batchHint": {
    en: "Several new paragraphs share one request, so fewer requests reach the provider. Edited paragraphs are always sent alone.",
    zh: "多个新段落合并成一次请求，发给接口的请求数随之减少。改动过的段落始终单独发送。",
  },
  "settings.parallel": { en: "Parallel requests · {n}", zh: "并行请求数 · {n}" },
  "settings.matchPool": {
    en: "Use the whole key pool ({keys} keys × {per} = {n})",
    zh: "按号池自动设置（{keys} 个密钥 × 每个 {per} 路 = {n}）",
  },

  // ── Key pool ──────────────────────────────────────────────────────
  "keys.title": { en: "Key pool", zh: "密钥号池" },
  "keys.count.one": { en: "1 key", zh: "1 个密钥" },
  "keys.count.many": { en: "{n} keys", zh: "{n} 个密钥" },
  "keys.manage": { en: "Manage keys…", zh: "管理号池…" },
  "keys.inKeychain": { en: "{n} in the keychain", zh: "钥匙串中有 {n} 个" },
  "keys.perKey": { en: "Requests per key", zh: "每个密钥的并发" },
  "keys.unlimited": { en: "No limit", zh: "不限" },
  "keys.retries": { en: "Retries", zh: "失败重试次数" },
  "keys.retriesDefault": { en: "Default (5)", zh: "默认（5 次）" },
  "keys.colName": { en: "Name", zh: "名称" },
  "keys.colKey": { en: "Key", zh: "密钥" },
  "keys.colState": { en: "State", zh: "状态" },
  "keys.colBusy": { en: "In flight", zh: "进行中" },
  "keys.state.ready": { en: "ready", zh: "可用" },
  "keys.state.idle": { en: "not used yet", zh: "尚未使用" },
  "keys.state.cooling": { en: "cooling down", zh: "限流冷却中" },
  "keys.state.rejected": { en: "set aside", zh: "已暂停使用" },
  "keys.namePlaceholder": { en: "name", zh: "名称" },
  "keys.paste": {
    en: "Paste keys: one per line, or a name on one line and its key on the next (name: key works too).",
    zh: "粘贴密钥：每行一个，或者一行名称、下一行密钥（也可以写成 名称: 密钥）。",
  },
  "keys.add": { en: "Add keys", zh: "追加密钥" },
  "keys.replace": { en: "Replace all", zh: "全部替换" },
  "keys.saveFirst": { en: "Save keys", zh: "保存密钥" },
  "keys.removeAll": { en: "Remove all", zh: "全部删除" },
  "keys.stored": { en: "{n} key(s) in the system keychain.", zh: "系统钥匙串中现有 {n} 个密钥。" },
  "keys.hint": {
    en: "Keys stay in the system keychain and are never shown again. Requests spread over the pool: the least busy ready key goes first, a key that is rate limited or rejected hands over to the next.",
    zh: "密钥保存在系统钥匙串中，之后不再显示。请求在号池中均匀分配，优先使用最空闲的可用密钥，某个密钥被限流或拒绝时自动换下一个。",
  },
  "keys.empty": { en: "No keys yet.", zh: "还没有密钥。" },
  "keys.testOk": { en: "Key {n} works: {text}", zh: "密钥 {n} 可用：{text}" },
  "keys.parallelNote": {
    en: "{n} requests can run at once ({keys} keys × {per}).",
    zh: "最多可同时发出 {n} 个请求（{keys} 个密钥 × 每个 {per} 路）。",
  },
} as const;
