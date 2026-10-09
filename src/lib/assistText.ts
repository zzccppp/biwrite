// Pure helpers of the writing assistant (tested with node --test).

/** Index of `needle` in `hay` if it occurs exactly once, else -1. */
export function uniqueIndex(hay: string, needle: string): number {
  if (!needle) return -1;
  const first = hay.indexOf(needle);
  if (first < 0) return -1;
  return hay.indexOf(needle, first + 1) < 0 ? first : -1;
}

/** The instruction that asks the model to bring an approved revision onto
 * text the author has edited since. */
export function reapplyInstruction(earlier: string, approved: string): string {
  return [
    "The author approved a revision of an earlier version of this text.",
    `<earlier_version>\n${earlier}\n</earlier_version>`,
    `<approved_version>\n${approved}\n</approved_version>`,
    "The text in <target> is the current version: the author has edited it since. Apply the changes the approved version made to the current text, and keep every later edit of the author.",
  ].join("\n");
}
