// The stretches in which two texts differ, so that putting one in place of
// the other leaves the rest (and positions in it) untouched.

export interface Change {
  from: number;
  to: number;
  insert: string;
}

/** Lines matched against each other at most (pairs of lines): beyond it,
 * the whole stretch between the first and the last difference goes as one. */
const MOST_PAIRS = 4_000_000;

const highSurrogate = (c: number) => c >= 0xd800 && c <= 0xdbff;
const lowSurrogate = (c: number) => c >= 0xdc00 && c <= 0xdfff;

/** The change that turns `old` into `text`: its common start and end kept,
 * the stretch between replaced, never splitting a character outside the
 * Basic Multilingual Plane. `null` when they are the same. */
export function differingStretch(old: string, text: string): Change | null {
  const most = Math.min(old.length, text.length);
  let start = 0;
  while (start < most && old.charCodeAt(start) === text.charCodeAt(start)) start++;
  if (start > 0 && highSurrogate(old.charCodeAt(start - 1))) start--;
  let end = 0;
  while (end < most - start && old.charCodeAt(old.length - 1 - end) === text.charCodeAt(text.length - 1 - end)) end++;
  if (end > 0 && lowSurrogate(old.charCodeAt(old.length - end))) end--;
  if (start === old.length - end && start === text.length - end) return null;
  return { from: start, to: old.length - end, insert: text.slice(start, text.length - end) };
}

/** The lines of `text`, each with its line break. */
function lines(text: string): string[] {
  return text.match(/[^\n]*\n|[^\n]+$/g) ?? [];
}

/** Where each line starts, and the end. */
function starts(lines: string[]): number[] {
  const out = [0];
  for (const line of lines) out.push(out[out.length - 1] + line.length);
  return out;
}

/** The changes that turn `old` into `text`, in order and apart: lines the
 * two share stay, each run of differing lines is one change, narrowed to
 * the characters that differ. Two edits far apart are two changes. */
export function differingStretches(old: string, text: string): Change[] {
  if (old === text) return [];
  const a = lines(old);
  const b = lines(text);
  let head = 0;
  while (head < a.length && head < b.length && a[head] === b[head]) head++;
  let tail = 0;
  while (tail < a.length - head && tail < b.length - head && a[a.length - 1 - tail] === b[b.length - 1 - tail]) tail++;
  const n = a.length - head - tail;
  const m = b.length - head - tail;
  // Runs of differing lines: [old from, old to, new from, new to).
  const runs: [number, number, number, number][] = [];
  if (n === 0 || m === 0 || (n + 1) * (m + 1) > MOST_PAIRS) {
    runs.push([head, head + n, head, head + m]);
  } else {
    // Longest common run of lines from (i, j) on, for the lines between.
    const width = m + 1;
    const common = new Uint32Array((n + 1) * width);
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        common[i * width + j] =
          a[head + i] === b[head + j]
            ? common[(i + 1) * width + j + 1] + 1
            : Math.max(common[(i + 1) * width + j], common[i * width + j + 1]);
      }
    }
    let i = 0;
    let j = 0;
    let run: [number, number] | null = null;
    const close = () => {
      if (run) runs.push([head + run[0], head + i, head + run[1], head + j]);
      run = null;
    };
    while (i < n || j < m) {
      if (i < n && j < m && a[head + i] === b[head + j]) {
        close();
        i++;
        j++;
        continue;
      }
      run ??= [i, j];
      if (j >= m || (i < n && common[(i + 1) * width + j] >= common[i * width + j + 1])) i++;
      else j++;
    }
    close();
  }
  const sa = starts(a);
  const sb = starts(b);
  const out: Change[] = [];
  for (const [a0, a1, b0, b1] of runs) {
    const change = differingStretch(old.slice(sa[a0], sa[a1]), text.slice(sb[b0], sb[b1]));
    if (change) out.push({ from: sa[a0] + change.from, to: sa[a0] + change.to, insert: change.insert });
  }
  return out;
}
