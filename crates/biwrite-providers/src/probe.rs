//! The short translations of a squeeze and of keeping a key alive: a few
//! words in a LaTeX shape, asked for in one of many wordings, with one of
//! several short system prompts, different every time, so the requests do
//! not repeat one text or one prompt. The words, section titles and
//! formulas come from the sample papers TeX Live ships (the ACM, IEEE and
//! Elsevier samples, `sample2e.tex`, `small2e.tex` and the `amsmath` test
//! file). Each request is a handful of words and a short prompt, as cheap
//! as the fixed translation it replaces.

use crate::config::{Effort, ProviderConfig};
use crate::keys::random_u64;

const WORDS: &str = include_str!("probe/words.txt");
const TITLES: &str = include_str!("probe/titles.txt");
const MATHS: &str = include_str!("probe/maths.txt");

/// Longest text, in characters.
pub(crate) const MAX_CHARS: usize = 120;

/// System prompts, each asking for the translation alone (a short answer).
const SYSTEMS: &[&str] = &[
    "Translate the user's text into Chinese. Reply with the translation only.",
    "You are a translator for academic papers. Answer with the Chinese translation and nothing else.",
    "You translate English LaTeX into Chinese. Keep commands and math as they are. Output only the result.",
    "Reply in Chinese with a translation of the text, nothing more.",
    "你是学术论文翻译助手，只输出中文译文。",
    "把用户给出的内容译成中文，保留 LaTeX 命令，只回复译文。",
    "你负责英译中，回答只包含译文。",
    "Translator. Chinese output only.",
    "You help with paper writing. When asked to translate, give only the translation.",
    "将英文翻译为简体中文，不要解释。",
    "Keep the answer short: the Chinese translation only.",
    "You are a concise English to Chinese translator.",
];

/// Ways to ask, `{t}` standing for the text.
const ASKS: &[&str] = &[
    "请帮我翻译：{t}",
    "请帮我翻译一下\n{t}",
    "把这些翻译成中文：{t}",
    "把下面这段 LaTeX 翻成中文\n{t}",
    "翻译成中文：{t}",
    "{t}\n\n翻译成中文",
    "{t}\n这个中文怎么说？",
    "帮忙译成中文，保留 LaTeX 命令：{t}",
    "中文翻译：{t}",
    "这段话的中文意思是？\n{t}",
    "英译中：{t}",
    "pls translate {t}",
    "pls translate to chinese: {t}",
    "Translate into Chinese:\n{t}",
    "Please translate the following into Chinese.\n\n{t}",
    "Could you put this into Chinese? {t}",
    "Chinese, please: {t}",
    "{t}\n\n(translate to Chinese)",
    "EN to ZH: {t}",
    "How would you say this in Chinese?\n{t}",
    "translate: {t}",
    "Help me translate this LaTeX snippet into Chinese, keep the commands.\n{t}",
    "Render in Chinese:\n{t}",
    "zh? {t}",
];

fn lines(list: &'static str) -> Vec<&'static str> {
    list.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

/// A small random source (xorshift), seeded per text.
struct Dice(u64);

impl Dice {
    fn new() -> Self {
        Self(random_u64() | 1)
    }

    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % n.max(1) as u64).unwrap_or(0)
    }

    fn pick(&mut self, list: &[&'static str]) -> &'static str {
        list[self.below(list.len())]
    }

    /// `low` to `high` words (inclusive).
    fn words(&mut self, list: &[&'static str], low: usize, high: usize) -> Vec<&'static str> {
        let n = low + self.below(high - low + 1);
        (0..n).map(|_| self.pick(list)).collect()
    }
}

fn capital(word: &str) -> String {
    let mut c = word.chars();
    c.next().map_or(String::new(), |f| {
        f.to_uppercase().collect::<String>() + c.as_str()
    })
}

fn sentence(words: &[&str]) -> String {
    format!("{}.", capital(&words.join(" ")))
}

fn title(words: &[&str]) -> String {
    words
        .iter()
        .map(|w| capital(w))
        .collect::<Vec<_>>()
        .join(" ")
}

/// `config` for short translations: little reasoning, so a few words cost
/// a few tokens (a provider that sends no effort still sends none).
pub(crate) fn config(config: &ProviderConfig) -> ProviderConfig {
    let mut config = config.clone();
    if config.effort != Effort::Default {
        config.effort = Effort::Low;
    }
    config
}

/// The system prompt and the user's message of one short translation.
pub(crate) fn prompt() -> (String, String) {
    let mut d = Dice::new();
    let text = text();
    let system = SYSTEMS[d.below(SYSTEMS.len())].to_owned();
    let one_line = !text.contains('\n');
    let wrapped = match d.below(8) {
        0 if one_line => format!("\"{text}\""),
        1 if one_line => format!("“{text}”"),
        2 if one_line => format!("「{text}」"),
        3 if one_line => format!("`{text}`"),
        4 => format!("```latex\n{text}\n```"),
        5 => format!("```\n{text}\n```"),
        6 => format!("<text>\n{text}\n</text>"),
        _ => text,
    };
    let user = ASKS[d.below(ASKS.len())].replace("{t}", &wrapped);
    (system, user)
}

/// A few words from the lists, in one of several LaTeX shapes.
pub(crate) fn text() -> String {
    let (words, titles, maths) = (lines(WORDS), lines(TITLES), lines(MATHS));
    let mut d = Dice::new();
    let w = d.words(&words, 2, 5);
    let out = match d.below(16) {
        0 => w.join(" "),
        1 => sentence(&w),
        2 => w.join(", "),
        3 => format!("\\section{{{}}}", d.pick(&titles)),
        4 => format!("\\subsection{{{}}}", title(&w[..2])),
        5 => format!("\\textbf{{{}}}: {}", capital(w[0]), sentence(&w[1..])),
        6 => format!("\\emph{{{}}} {}", w[0], w[1..].join(" ")),
        7 => format!(
            "\\begin{{itemize}}\n\\item {}\n\\item {}\n\\end{{itemize}}",
            w[0],
            w[1..].join(" ")
        ),
        8 => format!("\\caption{{{}}}", sentence(&w)),
        9 => format!(
            "{} ${}$ {}.",
            capital(w[0]),
            d.pick(&maths),
            w[1..].join(" ")
        ),
        10 => format!("${}$ {}", d.pick(&maths), w.join(" ")),
        11 => format!(
            "{} \\\\",
            w.iter().map(|x| capital(x)).collect::<Vec<_>>().join(" & ")
        ),
        12 => format!("\\paragraph{{{}}} {}", sentence(&w[..1]), sentence(&w[1..])),
        13 => format!("{} ({})", w[1..].join(" "), w[0]),
        14 => format!("\\footnote{{{}}}", sentence(&w)),
        _ => d.pick(&titles).to_owned(),
    };
    out.chars().take(MAX_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn the_lists_hold_words_titles_and_formulas() {
        assert!(lines(WORDS).len() > 300);
        assert!(lines(TITLES).len() > 20);
        assert!(lines(MATHS).len() > 60);
        assert!(
            lines(WORDS)
                .iter()
                .all(|w| w.chars().all(|c| c.is_ascii_lowercase()))
        );
    }

    #[test]
    fn prompts_vary_in_wording_and_stay_short() {
        let prompts: Vec<(String, String)> = (0..400).map(|_| prompt()).collect();
        let systems: HashSet<&String> = prompts.iter().map(|(s, _)| s).collect();
        assert_eq!(systems.len(), SYSTEMS.len(), "every system prompt turns up");
        let opening: HashSet<String> = prompts
            .iter()
            .map(|(_, u)| u.chars().take(6).collect())
            .collect();
        assert!(opening.len() > 20, "{} openings", opening.len());
        for (system, user) in &prompts {
            assert!(!system.is_empty() && !user.is_empty());
            assert!(!user.contains("{t}"));
            assert!(user.chars().count() < MAX_CHARS + 90, "{user}");
        }
        // Chinese and English asks both turn up.
        assert!(prompts.iter().any(|(_, u)| u.contains("翻译")));
        assert!(
            prompts
                .iter()
                .any(|(_, u)| u.to_lowercase().contains("translate"))
        );
    }

    #[test]
    fn short_translations_ask_for_little_reasoning() {
        let mut c = ProviderConfig {
            id: "p".into(),
            name: "P".into(),
            kind: crate::config::ProviderKind::OpenaiCompatible,
            base_url: "https://api.example.com/v1".into(),
            model: "m".into(),
            temperature: 0.0,
            effort: Effort::High,
            wire_api: crate::config::WireApi::Responses,
            service_tier: None,
            key_concurrency: None,
            max_retries: None,
        };
        assert_eq!(config(&c).effort, Effort::Low);
        assert_eq!(config(&c).model, "m");
        c.effort = Effort::Default;
        assert_eq!(config(&c).effort, Effort::Default, "none sent stays none");
    }

    #[test]
    fn texts_are_short_varied_and_balanced() {
        let texts: Vec<String> = (0..400).map(|_| text()).collect();
        let distinct: HashSet<&String> = texts.iter().collect();
        assert!(distinct.len() > 350, "{} distinct", distinct.len());
        for t in &texts {
            assert!(!t.trim().is_empty());
            assert!(t.chars().count() <= MAX_CHARS, "{t}");
        }
        // Whole texts keep their braces and dollars paired (one cut at
        // the limit may not).
        let whole: Vec<&String> = texts
            .iter()
            .filter(|t| t.chars().count() < MAX_CHARS)
            .collect();
        for t in &whole {
            assert_eq!(t.matches('{').count(), t.matches('}').count(), "{t}");
            assert_eq!(t.matches('$').count() % 2, 0, "{t}");
        }
        // Several shapes turn up.
        let shapes = ["\\section{", "\\item ", "$", " & ", "\\caption{", ", "];
        for s in shapes {
            assert!(texts.iter().any(|t| t.contains(s)), "no text with {s}");
        }
    }
}
