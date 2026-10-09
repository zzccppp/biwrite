//! Cleaning model output down to the translation itself.
//!
//! Models sometimes wrap the answer: reasoning in `<think>` blocks, code
//! fences, XML-ish tags, a "Translation:" / "译文：" label, or quotes. The
//! final text is cleaned with [`clean_output`]; while streaming, [`ThinkFilter`]
//! hides a leading reasoning block so it never flashes in the pane.

const THINK_TAGS: &[(&str, &str)] = &[("<think>", "</think>"), ("<thinking>", "</thinking>")];
const WRAPPER_TAGS: &[&str] = &[
    "translation",
    "output",
    "result",
    "answer",
    "source",
    "text",
];
const LABELS: &[&str] = &[
    "here is the translation",
    "here's the translation",
    "chinese translation",
    "english translation",
    "translated text",
    "translation",
    "以下是翻译",
    "中文翻译",
    "英文翻译",
    "译文",
    "翻译",
];
const QUOTES: &[(char, char)] = &[('"', '"'), ('“', '”'), ('「', '」'), ('『', '』')];

/// Strip wrappers the model added around the translation of `source`.
pub fn clean_output(text: &str, source: &str) -> String {
    let mut t = strip_think(text).trim().to_owned();
    t = strip_fence(&t);
    t = strip_wrapper_tag(&t);
    if label_len(source.trim()).is_none() {
        t = strip_label(&t);
    }
    t = strip_quotes(&t, source);
    t.trim().to_owned()
}

/// The text after any reasoning blocks at its start (`<think>…</think>`).
pub fn strip_reasoning(text: &str) -> &str {
    strip_think(text)
}

/// Remove reasoning blocks at the start (complete ones only).
fn strip_think(text: &str) -> &str {
    let mut t = text.trim_start();
    loop {
        let Some((open, close)) = THINK_TAGS.iter().find(|(open, _)| t.starts_with(open)) else {
            return t;
        };
        match t.find(close) {
            Some(end) => t = t[end + close.len()..].trim_start(),
            None => return &t[open.len()..], // unterminated: keep what follows the tag
        }
    }
}

fn strip_fence(t: &str) -> String {
    let Some(rest) = t.strip_prefix("```") else {
        return t.to_owned();
    };
    let Some(body_start) = rest.find('\n') else {
        return t.to_owned();
    };
    let body = &rest[body_start + 1..];
    match body.trim_end().strip_suffix("```") {
        Some(inner) => inner.trim().to_owned(),
        None => t.to_owned(),
    }
}

fn strip_wrapper_tag(t: &str) -> String {
    for tag in WRAPPER_TAGS {
        let (open, close) = (format!("<{tag}>"), format!("</{tag}>"));
        if let Some(inner) = t
            .strip_prefix(&open)
            .and_then(|r| r.trim_end().strip_suffix(&close))
        {
            return inner.trim().to_owned();
        }
    }
    t.to_owned()
}

/// Byte length of a leading "Translation:" / "译文：" label (optionally with a
/// parenthetical, "Translation (simplified):"), if `t` starts with one.
fn label_len(t: &str) -> Option<usize> {
    let first = &t[..t.find('\n').unwrap_or(t.len())];
    // ASCII-only lowercasing keeps byte offsets valid for slicing `first`.
    let lower = first.to_ascii_lowercase();
    LABELS.iter().find_map(|label| {
        let mut after = lower.strip_prefix(label)?.trim_start();
        if let Some(inner) = after.strip_prefix(['(', '（']) {
            let close = inner.find([')', '）'])?;
            after = inner[close..].trim_start_matches([')', '）']).trim_start();
        }
        let rest = after.strip_prefix([':', '：'])?;
        Some(first.len() - rest.len())
    })
}

/// Drop a leading label the model added (callers skip this when the source
/// itself starts with such a label, so it belongs to the text).
fn strip_label(t: &str) -> String {
    match label_len(t) {
        Some(len) => t[len..].trim().to_owned(),
        None => t.to_owned(),
    }
}

fn strip_quotes(t: &str, source: &str) -> String {
    let src = source.trim();
    for (open, close) in QUOTES {
        if let Some(inner) = t.strip_prefix(*open).and_then(|r| r.strip_suffix(*close)) {
            let source_quoted =
                src.starts_with(*open) || src.starts_with(['"', '“', '「', '『', '\'', '`', '‘']);
            if !source_quoted && !inner.contains(*open) && !inner.contains(*close) {
                return inner.to_owned();
            }
        }
    }
    t.to_owned()
}

/// Streaming filter that withholds a leading `<think>…</think>` block.
#[derive(Debug, Default)]
pub struct ThinkFilter {
    raw: String,
    /// Byte offset in `raw` up to which output has been emitted (or skipped).
    emitted: usize,
    decided: bool,
}

impl ThinkFilter {
    /// Add a streamed fragment; returns text that can be shown now.
    pub fn push(&mut self, delta: &str) -> Option<String> {
        self.raw.push_str(delta);
        if !self.decided {
            let lead = self.raw.len() - self.raw.trim_start().len();
            let t = &self.raw[lead..];
            if t.is_empty()
                || THINK_TAGS
                    .iter()
                    .any(|(open, _)| open.starts_with(t) && t.len() < open.len())
            {
                return None; // could still become a think tag
            }
            match THINK_TAGS.iter().find(|(open, _)| t.starts_with(open)) {
                Some((_, close)) => {
                    let end = t.find(close)?;
                    let after = lead + end + close.len();
                    let skip_ws = self.raw[after..].len() - self.raw[after..].trim_start().len();
                    self.emitted = after + skip_ws;
                    self.decided = true;
                }
                None => self.decided = true,
            }
        }
        if self.emitted >= self.raw.len() {
            return None;
        }
        let out = self.raw[self.emitted..].to_owned();
        self.emitted = self.raw.len();
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_output_is_untouched() {
        assert_eq!(clean_output("  图神经网络。 \n", "GNNs."), "图神经网络。");
    }

    #[test]
    fn strips_wrappers() {
        assert_eq!(clean_output("<think>hmm\nok</think>\n\n结果", "x"), "结果");
        assert_eq!(clean_output("```\n结果\n```", "x"), "结果");
        assert_eq!(clean_output("```markdown\n结果\n```", "x"), "结果");
        assert_eq!(clean_output("<translation>结果</translation>", "x"), "结果");
        assert_eq!(clean_output("Translation: 结果", "x"), "结果");
        assert_eq!(clean_output("译文：\n结果", "x"), "结果");
        assert_eq!(
            clean_output("Chinese translation (simplified): 结果", "x"),
            "结果"
        );
        assert_eq!(clean_output("“结果”", "Result"), "结果");
    }

    #[test]
    fn keeps_quotes_and_labels_that_belong_to_the_text() {
        assert_eq!(clean_output("“结果”", "\"Result\""), "“结果”");
        assert_eq!(
            clean_output("“少即是多。”", "``Less is more.''"),
            "“少即是多。”"
        );
        assert_eq!(
            clean_output(
                "翻译：我们在 WMT14 上评估。",
                "Translation: we evaluate on WMT14."
            ),
            "翻译：我们在 WMT14 上评估。"
        );
        assert_eq!(
            clean_output("Translation: we evaluate.", "译文：我们评估。"),
            "Translation: we evaluate."
        );
        assert_eq!(clean_output("“甲”和“乙”", "A and B"), "“甲”和“乙”");
        assert_eq!(clean_output("翻译模型很强。", "x"), "翻译模型很强。");
    }

    #[test]
    fn think_filter_hides_reasoning_while_streaming() {
        let mut f = ThinkFilter::default();
        assert_eq!(f.push("<thi"), None);
        assert_eq!(f.push("nk>let me think"), None);
        assert_eq!(f.push("...</think>\n\n结"), Some("结".into()));
        assert_eq!(f.push("果"), Some("果".into()));

        let mut g = ThinkFilter::default();
        assert_eq!(g.push("<"), None);
        assert_eq!(g.push("b>粗体"), Some("<b>粗体".into()));
        let mut h = ThinkFilter::default();
        assert_eq!(h.push("结果"), Some("结果".into()));
    }
}
