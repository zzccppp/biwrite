// Interface language (English or Chinese). Every UI string is a key in
// `messages` with both languages side by side. `t()` reads the language
// state, so text in templates updates as soon as the language changes.
// English stays the default so existing users see the same interface.

import { messages } from "./messages";

export type Lang = "en" | "zh";
export type MessageKey = keyof typeof messages;

const LANG_KEY = "biwrite.lang";

function loadLang(): Lang {
  try {
    return localStorage.getItem(LANG_KEY) === "zh" ? "zh" : "en";
  } catch {
    return "en";
  }
}

class Language {
  current = $state<Lang>(loadLang());

  set(lang: Lang): void {
    this.current = lang;
    document.documentElement.lang = lang === "zh" ? "zh-CN" : "en";
    try {
      localStorage.setItem(LANG_KEY, lang);
    } catch {
      // Not persisted; harmless.
    }
  }
}

export const language = new Language();

/** The message in the current language, with `{name}` placeholders filled. */
export function t(key: MessageKey, vars?: Record<string, string | number>): string {
  const entry = messages[key];
  let text: string = language.current === "zh" ? entry.zh : entry.en;
  if (vars) {
    for (const [name, value] of Object.entries(vars)) {
      text = text.replaceAll(`{${name}}`, String(value));
    }
  }
  return text;
}

/** Plural-aware count for English ("1 key", "3 keys"); Chinese has no plural. */
export function count(n: number, one: MessageKey, many: MessageKey): string {
  return t(n === 1 ? one : many, { n });
}
