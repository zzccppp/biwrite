// Pure state helpers for the editable model combobox in ProviderForm.
//
// Why not native `<input list>` + `<datalist>`?
// Browsers filter `<datalist>` options by the current `<input>` value, so when
// a provider already has a model filled in (e.g. from a preset or saved config),
// clicking the input hides all other fetched models until the user deletes the
// existing model name. Here, opening the dropdown (`filtering === false`)
// always displays the full list, while typing (`filtering === true`) filters
// dynamically and still allows entering or adding any custom model ID.

export interface ModelDropdownState {
  /** Models shown in the listbox (`all` when `filtering` is false). */
  options: string[];
  /** Offer an explicit "+ Add / Use <model>" row when typing a custom ID not in `all`. */
  canAddCustom: boolean;
  /** Trimmed custom model ID from the input. */
  customValue: string;
}

/** Combine custom and fetched model IDs, keeping fetched models in order and prepending custom-only ones. */
export function mergeModels(fetched: readonly string[], custom: readonly string[]): string[] {
  const fetchedSet = new Set(fetched.map((m) => m.trim()).filter(Boolean));
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of custom) {
    const m = raw.trim();
    if (m && !fetchedSet.has(m) && !seen.has(m)) {
      seen.add(m);
      out.push(m);
    }
  }
  for (const raw of fetched) {
    const m = raw.trim();
    if (m && !seen.has(m)) {
      seen.add(m);
      out.push(m);
    }
  }
  return out;
}

/**
 * Visible options for the model dropdown:
 * - When `filtering` is `false` (just fetched, clicked input/chevron, or Arrow key open),
 *   ALL models in `all` are returned even when `inputValue` already holds a model name.
 * - When `filtering` is `true` (user is typing), matches case-insensitively with
 *   prefix matches listed first, and exposes `canAddCustom` when `inputValue` is
 *   a new custom model ID not in `all`.
 */
export function visibleModels(
  all: readonly string[],
  inputValue: string,
  filtering: boolean,
): ModelDropdownState {
  const customValue = inputValue.trim();
  const isCustom = customValue.length > 0 && !all.includes(customValue);
  if (!filtering) {
    return { options: [...all], canAddCustom: false, customValue };
  }
  if (customValue === "") {
    return { options: [...all], canAddCustom: false, customValue: "" };
  }
  const needle = customValue.toLowerCase();
  const prefix: string[] = [];
  const rest: string[] = [];
  for (const m of all) {
    const lower = m.toLowerCase();
    if (lower.startsWith(needle)) prefix.push(m);
    else if (lower.includes(needle)) rest.push(m);
  }
  return { options: [...prefix, ...rest], canAddCustom: isCustom, customValue };
}

/** Step keyboard highlight up or down across `total` listbox rows (wrapping around). */
export function stepOptionIndex(current: number, delta: 1 | -1, total: number): number {
  if (total <= 0) return -1;
  if (current < 0 || current >= total) {
    return delta > 0 ? 0 : total - 1;
  }
  return (current + delta + total) % total;
}

/** Same HTTP(S) URL origin (`scheme://host:port`), matching Rust's `keeps_keys`. */
export function sameUrlOrigin(a: string, b: string): boolean {
  try {
    const ua = new URL(a.trim());
    const ub = new URL(b.trim());
    if (ua.protocol !== "https:" && ua.protocol !== "http:") return false;
    return ua.origin === ub.origin;
  } catch {
    return false;
  }
}

/** Loopback `http`/`https` base URL (such as local Ollama or LM Studio). */
export function isLoopbackBaseUrl(baseUrl: string): boolean {
  try {
    const u = new URL(baseUrl.trim());
    return (
      (u.protocol === "http:" || u.protocol === "https:") &&
      (u.hostname === "localhost" || u.hostname === "127.0.0.1" || u.hostname === "[::1]" || u.hostname === "::1")
    );
  } catch {
    return false;
  }
}
