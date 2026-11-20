// Editor and translation pane text size offset (in px), remembered per machine.

import type { Mode } from "./types";

export const FONT_SIZE_KEY = "biwrite.fontDelta";
/** Base prose font size in px (`styles.css`: `--source-size`). */
export const BASE_PROSE_SIZE = 16;
/** Base LaTeX source font size in px (`App.svelte`: `.source[data-mode="latex"]`). */
export const BASE_LATEX_SIZE = 14;
/** Minimum offset (-5 px -> 11 px prose, 9 px LaTeX). */
export const MIN_FONT_DELTA = -5;
/** Maximum offset (+16 px -> 32 px prose, 30 px LaTeX). */
export const MAX_FONT_DELTA = 16;

export type FontZoomAction = "in" | "out" | "reset";

export interface KeyLike {
  key: string;
  code: string;
  metaKey: boolean;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
}

export function clampFontDelta(delta: number): number {
  if (!Number.isFinite(delta)) return 0;
  return Math.min(MAX_FONT_DELTA, Math.max(MIN_FONT_DELTA, Math.round(delta)));
}

export function loadFontDelta(): number {
  try {
    const raw = localStorage.getItem(FONT_SIZE_KEY);
    if (raw === null) return 0;
    return clampFontDelta(parseInt(raw, 10));
  } catch {
    return 0;
  }
}

export function applyFontDelta(delta: number, persist = true): number {
  const clamped = clampFontDelta(delta);
  if (typeof document !== "undefined") {
    const root = document.documentElement;
    if (clamped === 0) root.style.removeProperty("--font-delta");
    else root.style.setProperty("--font-delta", `${clamped}px`);
  }
  if (persist) {
    try {
      localStorage.setItem(FONT_SIZE_KEY, String(clamped));
    } catch {
      // Storage unavailable: the choice stays for this session only.
    }
  }
  return clamped;
}

export function editorFontSize(mode: Mode, delta: number): number {
  const base = mode === "latex" ? BASE_LATEX_SIZE : BASE_PROSE_SIZE;
  return base + clampFontDelta(delta);
}

/** Detect macOS from `navigator.platform` / `navigator.userAgent`. */
export function isMacPlatform(nav: { platform?: string; userAgent?: string } = navigator): boolean {
  const raw = `${nav.platform ?? ""} ${nav.userAgent ?? ""}`.toLowerCase();
  return raw.includes("mac");
}

/**
 * Platform-aware shortcut for adjusting editor font size:
 * - macOS: Cmd + (`=` / `+` / NumpadAdd), Cmd - (`-` / `_` / NumpadSubtract), Cmd 0
 * - Windows / Linux: Ctrl + (`=` / `+` / NumpadAdd), Ctrl - (`-` / `_` / NumpadSubtract), Ctrl 0
 * Rejects IME composition, extra primary/Alt modifiers (Ctrl on macOS, Win or AltGr on Windows),
 * and Numpad0 when NumLock is off (`Insert`, so Ctrl+Insert Copy on Windows/Linux stays intact).
 */
export function fontShortcutAction(e: KeyLike, isMac: boolean): FontZoomAction | null {
  if (e.isComposing || e.keyCode === 229 || e.key === "Process" || e.key === "Dead") return null;
  const primary = isMac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
  if (!primary || e.altKey) return null;

  if (e.key === "+" || e.key === "=" || e.key === "＋" || e.key === "＝" || e.code === "NumpadAdd") {
    return "in";
  }
  if (e.key === "-" || e.key === "_" || e.key === "－" || e.key === "—" || e.code === "NumpadSubtract") {
    return "out";
  }
  if (e.key === "0" || e.key === "０") {
    return "reset";
  }

  // Physical-key fallbacks only when a non-Latin script layout (e.g. Cyrillic, Arabic, Hebrew)
  // produces a non-Latin single character on physical `Equal` / `Minus` / `Digit0`.
  // Printable ASCII and Latin layouts already map `+`, `=`, `-`, `_`, `0` directly in `e.key`,
  // so symbols like `?`, `ß`, `)`, `à`, or `Insert` on those physical keys are never hijacked.
  const isNonLatinChar = [...e.key].length === 1 && (e.key.codePointAt(0) ?? 0) >= 0x0370;
  if (isNonLatinChar) {
    if (e.code === "Equal") return "in";
    if (e.code === "Minus") return "out";
    if (!e.shiftKey && e.code === "Digit0") return "reset";
  }
  return null;
}
