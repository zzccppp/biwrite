import assert from "node:assert/strict";
import test from "node:test";
import {
  BASE_LATEX_SIZE,
  BASE_PROSE_SIZE,
  MAX_FONT_DELTA,
  MIN_FONT_DELTA,
  clampFontDelta,
  editorFontSize,
  fontShortcutAction,
  isMacPlatform,
  type KeyLike,
} from "./fontSize.ts";

function keyEvent(overrides: Partial<KeyLike>): KeyLike {
  return {
    key: "",
    code: "",
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...overrides,
  };
}

test("fontSize: macOS uses Cmd (+/- and 0) and ignores Ctrl", () => {
  // Cmd + `=` (unshifted), Cmd + Shift + `=` (`+`), full-width `＋`/`＝`, and NumpadAdd increase size.
  assert.equal(fontShortcutAction(keyEvent({ key: "=", code: "Equal", metaKey: true }), true), "in");
  assert.equal(
    fontShortcutAction(keyEvent({ key: "+", code: "Equal", metaKey: true, shiftKey: true }), true),
    "in",
  );
  assert.equal(fontShortcutAction(keyEvent({ key: "＝", code: "Equal", metaKey: true }), true), "in");
  assert.equal(fontShortcutAction(keyEvent({ key: "+", code: "NumpadAdd", metaKey: true }), true), "in");

  // Cmd + `-`, Cmd + Shift + `-` (`_`), full-width `—`, and NumpadSubtract decrease size.
  assert.equal(fontShortcutAction(keyEvent({ key: "-", code: "Minus", metaKey: true }), true), "out");
  assert.equal(
    fontShortcutAction(keyEvent({ key: "_", code: "Minus", metaKey: true, shiftKey: true }), true),
    "out",
  );
  assert.equal(fontShortcutAction(keyEvent({ key: "—", code: "Minus", metaKey: true }), true), "out");
  assert.equal(fontShortcutAction(keyEvent({ key: "-", code: "NumpadSubtract", metaKey: true }), true), "out");

  // Cmd + `0` resets, while Cmd + Shift + `0` (`)`) does not.
  assert.equal(fontShortcutAction(keyEvent({ key: "0", code: "Digit0", metaKey: true }), true), "reset");
  assert.equal(fontShortcutAction(keyEvent({ key: "0", code: "Numpad0", metaKey: true }), true), "reset");
  assert.equal(
    fontShortcutAction(keyEvent({ key: ")", code: "Digit0", metaKey: true, shiftKey: true }), true),
    null,
  );

  // Ctrl on macOS is NOT the shortcut modifier, nor is Cmd+Ctrl or Cmd+Option.
  assert.equal(fontShortcutAction(keyEvent({ key: "=", code: "Equal", ctrlKey: true }), true), null);
  assert.equal(fontShortcutAction(keyEvent({ key: "-", code: "Minus", ctrlKey: true }), true), null);
  assert.equal(fontShortcutAction(keyEvent({ key: "0", code: "Digit0", ctrlKey: true }), true), null);
  assert.equal(
    fontShortcutAction(keyEvent({ key: "=", code: "Equal", metaKey: true, ctrlKey: true }), true),
    null,
  );
  assert.equal(
    fontShortcutAction(keyEvent({ key: "=", code: "Equal", metaKey: true, altKey: true }), true),
    null,
  );
});

test("fontSize: Windows and Linux use Ctrl (+/- and 0) and preserve Copy, IME, and international layouts", () => {
  // Ctrl + `=` and Ctrl + Shift + `=` (`+`) increase size.
  assert.equal(fontShortcutAction(keyEvent({ key: "=", code: "Equal", ctrlKey: true }), false), "in");
  assert.equal(
    fontShortcutAction(keyEvent({ key: "+", code: "Equal", ctrlKey: true, shiftKey: true }), false),
    "in",
  );
  assert.equal(fontShortcutAction(keyEvent({ key: "+", code: "NumpadAdd", ctrlKey: true }), false), "in");
  // International layout where `+` sits on another key (e.g. German BracketRight).
  assert.equal(fontShortcutAction(keyEvent({ key: "+", code: "BracketRight", ctrlKey: true }), false), "in");

  // Ctrl + `-` and Ctrl + Shift + `-` decrease size.
  assert.equal(fontShortcutAction(keyEvent({ key: "-", code: "Minus", ctrlKey: true }), false), "out");
  assert.equal(
    fontShortcutAction(keyEvent({ key: "_", code: "Minus", ctrlKey: true, shiftKey: true }), false),
    "out",
  );
  assert.equal(fontShortcutAction(keyEvent({ key: "-", code: "Slash", ctrlKey: true }), false), "out");

  // Ctrl + `0` resets, while Ctrl + Insert (Numpad0 with NumLock OFF) stays untouched for Copy.
  assert.equal(fontShortcutAction(keyEvent({ key: "0", code: "Digit0", ctrlKey: true }), false), "reset");
  assert.equal(fontShortcutAction(keyEvent({ key: "0", code: "Numpad0", ctrlKey: true }), false), "reset");
  assert.equal(fontShortcutAction(keyEvent({ key: "Insert", code: "Numpad0", ctrlKey: true }), false), null);

  // IME composition events are never hijacked.
  assert.equal(
    fontShortcutAction(keyEvent({ key: "Process", code: "Equal", ctrlKey: true, isComposing: true, keyCode: 229 }), false),
    null,
  );
  assert.equal(
    fontShortcutAction(keyEvent({ key: "=", code: "Equal", metaKey: true, isComposing: true }), true),
    null,
  );
  assert.equal(fontShortcutAction(keyEvent({ key: "Dead", code: "Equal", ctrlKey: true }), false), null);

  // Win key (`metaKey`), AltGr (`ctrlKey + altKey`), AZERTY `)`, Nordic `?`, and German `ß` are ignored.
  assert.equal(fontShortcutAction(keyEvent({ key: "+", code: "Equal", metaKey: true }), false), null);
  assert.equal(
    fontShortcutAction(keyEvent({ key: "~", code: "BracketRight", ctrlKey: true, altKey: true }), false),
    null,
  );
  assert.equal(fontShortcutAction(keyEvent({ key: ")", code: "Minus", ctrlKey: true }), false), null);
  assert.equal(
    fontShortcutAction(keyEvent({ key: "?", code: "Minus", ctrlKey: true, shiftKey: true }), false),
    null,
  );
  assert.equal(fontShortcutAction(keyEvent({ key: "ß", code: "Minus", ctrlKey: true }), false), null);

  // Non-Latin script (e.g. Cyrillic `ъ` on physical Equal) still falls back via physical key code.
  assert.equal(fontShortcutAction(keyEvent({ key: "ъ", code: "Equal", ctrlKey: true }), false), "in");
});

test("fontSize: clamping, mode-specific pixel sizes, and platform detection", () => {
  assert.equal(clampFontDelta(0), 0);
  assert.equal(clampFontDelta(NaN), 0);
  assert.equal(clampFontDelta(Infinity), 0);
  assert.equal(clampFontDelta(-99), MIN_FONT_DELTA);
  assert.equal(clampFontDelta(99), MAX_FONT_DELTA);
  assert.equal(clampFontDelta(2.6), 3);

  assert.equal(editorFontSize("plain", 0), BASE_PROSE_SIZE);
  assert.equal(editorFontSize("markdown", 3), BASE_PROSE_SIZE + 3);
  assert.equal(editorFontSize("latex", 0), BASE_LATEX_SIZE);
  assert.equal(editorFontSize("latex", 3), BASE_LATEX_SIZE + 3);
  assert.equal(editorFontSize("latex", -99), BASE_LATEX_SIZE + MIN_FONT_DELTA);

  assert.equal(isMacPlatform({ platform: "MacIntel" }), true);
  assert.equal(isMacPlatform({ platform: "", userAgent: "Mozilla/5.0 (Macintosh; ARM Mac OS X)" }), true);
  assert.equal(isMacPlatform({ platform: "Win32", userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)" }), false);
  assert.equal(isMacPlatform({ platform: "Linux x86_64" }), false);
});
