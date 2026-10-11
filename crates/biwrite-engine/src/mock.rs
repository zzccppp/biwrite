//! Mock translator for development: returns the reversed source after a
//! random delay, streaming it in a few chunks. Placeholders (`⟦n⟧`) are kept
//! whole, so protected math and citations survive like with a real model.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use biwrite_core::protect::placeholder_at;

use crate::random::random_between;
use crate::translator::{
    BoxFuture, PartialFn, TokenUsage, TranslateError, TranslationOutput, TranslationRequest,
    Translator,
};

/// Number of streamed fragments per response.
const STREAM_CHUNKS: usize = 6;

pub struct MockTranslator {
    /// Reverse the letters of words only, keeping LaTeX as it is
    /// ([`reverse_words`]). This is the offline provider of the app, whose
    /// translations must compile.
    keep_markup: bool,
    min_delay: Duration,
    max_delay: Duration,
    calls: AtomicU64,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl Default for MockTranslator {
    fn default() -> Self {
        Self::with_delay(Duration::from_millis(500), Duration::from_millis(2000))
    }
}

impl MockTranslator {
    /// A mock whose response time is uniform in `[min_delay, max_delay]`.
    pub fn with_delay(min_delay: Duration, max_delay: Duration) -> Self {
        Self {
            keep_markup: false,
            min_delay,
            max_delay: max_delay.max(min_delay),
            calls: AtomicU64::new(0),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        }
    }

    /// The app's offline provider, with words reversed letter by letter and
    /// LaTeX kept intact (see [`reverse_words`]).
    pub fn for_documents() -> Self {
        Self::default().keeping_markup()
    }

    /// This mock, reversing only the letters of words (see
    /// [`reverse_words`]).
    pub fn keeping_markup(mut self) -> Self {
        self.keep_markup = true;
        self
    }

    /// Number of `translate` calls made so far.
    pub fn calls(&self) -> u64 {
        self.calls.load(Ordering::SeqCst)
    }

    /// Highest number of concurrent requests observed.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

/// Decrements the in-flight counter even if the future is dropped (cancelled).
struct InFlight<'a>(&'a AtomicUsize);

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Translator for MockTranslator {
    fn provider(&self) -> &str {
        "mock"
    }

    fn model(&self) -> &str {
        // Its own name, so cached translations of the plain mock (whole
        // text reversed) are not taken for its own.
        if self.keep_markup {
            "reverse-words"
        } else {
            "reverse"
        }
    }

    fn is_placeholder(&self) -> bool {
        true
    }

    fn translate<'a>(
        &'a self,
        request: &'a TranslationRequest,
        on_partial: PartialFn<'a>,
    ) -> BoxFuture<'a, Result<TranslationOutput, TranslateError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(now, Ordering::SeqCst);
            let _guard = InFlight(&self.in_flight);

            let total = random_between(self.min_delay, self.max_delay);
            let reversed = if self.keep_markup {
                reverse_words(&request.source)
            } else {
                reverse(&request.source)
            };
            let chunks = split_chunks(&reversed, STREAM_CHUNKS);
            let step = total / u32::try_from(chunks.len().max(1)).unwrap_or(1);
            if chunks.is_empty() {
                tokio::time::sleep(total).await;
            }
            let mut shown = String::new();
            for chunk in &chunks {
                tokio::time::sleep(step).await;
                shown.push_str(chunk);
                on_partial(&shown);
            }
            Ok(TranslationOutput {
                usage: TokenUsage {
                    input_tokens: estimate_tokens(&request.source),
                    output_tokens: estimate_tokens(&reversed),
                },
                text: reversed,
            })
        })
    }
}

/// Reverse the characters, keeping each placeholder as one unit.
pub fn reverse(text: &str) -> String {
    let mut units = Vec::new();
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        let len = placeholder_at(rest).map_or(c.len_utf8(), |(len, _)| len);
        units.push(&rest[..len]);
        rest = &rest[len..];
    }
    units.reverse();
    units.concat()
}

/// Commands whose braced argument is running text.
const TEXT_COMMANDS: &[&str] = &[
    "textit",
    "textbf",
    "textsl",
    "textsc",
    "textrm",
    "textsf",
    "texttt",
    "emph",
    "underline",
    "footnote",
    "caption",
    "section",
    "subsection",
    "subsubsection",
    "paragraph",
    "subparagraph",
    "chapter",
    "part",
    "title",
    "mbox",
    "text",
];

/// Reverse the letters of each word of running text, and nothing else.
/// Command names, the arguments of other commands (environment names,
/// lengths, colours, file names), options in brackets, placeholders, digits
/// and punctuation stay as they are. The LaTeX keeps its structure, so a
/// document that compiles still does with these "translations".
pub fn reverse_words(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    // For each open brace, whether it holds running text.
    let mut groups: Vec<bool> = Vec::new();
    // The command just read, whose next brace is its argument.
    let mut command: Option<String> = None;
    let mut brackets = 0usize;
    let mut word: Vec<char> = Vec::new();
    let flush = |word: &mut Vec<char>, out: &mut String| {
        out.extend(word.iter().rev());
        word.clear();
    };
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let rest: String = chars[i..].iter().take(16).collect();
        if let Some((len, _)) = placeholder_at(&rest) {
            flush(&mut word, &mut out);
            let n = rest[..len].chars().count();
            out.extend(&chars[i..i + n]);
            i += n;
            command = None;
            continue;
        }
        let in_text = brackets == 0 && groups.iter().all(|&text| text);
        if c.is_alphabetic() && in_text {
            word.push(c);
            command = None;
            i += 1;
            continue;
        }
        flush(&mut word, &mut out);
        match c {
            '\\' => {
                let start = i + 1;
                let mut end = start;
                while end < chars.len() && chars[end].is_ascii_alphabetic() {
                    end += 1;
                }
                if end == start && end < chars.len() {
                    end += 1;
                }
                out.extend(&chars[i..end]);
                command = Some(chars[start..end].iter().collect());
                i = end;
                continue;
            }
            '{' => {
                let text = match command.take() {
                    Some(name) => TEXT_COMMANDS.contains(&name.as_str()),
                    None => true,
                };
                groups.push(text);
            }
            '}' => {
                groups.pop();
                command = None;
            }
            '[' if command.is_some() || brackets > 0 => brackets += 1,
            ']' if brackets > 0 => brackets -= 1,
            c if c.is_whitespace() => {}
            // Options in brackets belong to the command before them.
            _ if brackets > 0 => {}
            _ => command = None,
        }
        out.push(c);
        i += 1;
    }
    flush(&mut word, &mut out);
    out
}

/// Split into at most `n` pieces on char boundaries.
fn split_chunks(text: &str, n: usize) -> Vec<&str> {
    let chars = text.chars().count();
    if chars == 0 {
        return Vec::new();
    }
    let per = chars.div_ceil(n);
    let mut out = Vec::with_capacity(n);
    let mut start = 0;
    for (count, (idx, _)) in text.char_indices().enumerate() {
        if count > 0 && count % per == 0 {
            out.push(&text[start..idx]);
            start = idx;
        }
    }
    out.push(&text[start..]);
    out
}

/// Rough token estimate (~4 chars per token) for the mock's usage report.
fn estimate_tokens(text: &str) -> u64 {
    u64::try_from(text.chars().count().div_ceil(4)).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunks_reassemble() {
        let text = "数据集包含😀 emoji and text";
        assert_eq!(split_chunks(text, 6).concat(), text);
        assert!(split_chunks(text, 6).len() <= 6);
        assert!(split_chunks("", 6).is_empty());
        assert_eq!(split_chunks("ab", 6), vec!["a", "b"]);
    }

    #[test]
    fn placeholders_stay_whole() {
        assert_eq!(reverse("ab ⟦12⟧ c⟦0⟧"), "⟦0⟧c ⟦12⟧ ba");
    }

    /// Only the letters of running text move. The LaTeX around them, and
    /// every argument that is not text, stays as it was.
    #[test]
    fn reverse_words_keeps_latex_intact() {
        assert_eq!(
            reverse_words("Deep \\textit{Learning} is ⟦3⟧ fun, see~⟦12⟧."),
            "peeD \\textit{gninraeL} si ⟦3⟧ nuf, ees~⟦12⟧."
        );
        assert_eq!(
            reverse_words("\\begin{tabular}{ll} cat & dog \\\\ \\end{tabular}"),
            "\\begin{tabular}{ll} tac & god \\\\ \\end{tabular}"
        );
        assert_eq!(
            reverse_words("\\hspace{1cm}word \\color{red}{red text}\\\\[2pt]"),
            "\\hspace{1cm}drow \\color{red}{der txet}\\\\[2pt]"
        );
        assert_eq!(
            reverse_words("\\def\\arraystretch{1.5}"),
            "\\def\\arraystretch{1.5}"
        );
        assert_eq!(
            reverse_words("{\\bf bold} \\item[first] item \\includegraphics[width=3cm]{fig}"),
            "{\\bf dlob} \\item[first] meti \\includegraphics[width=3cm]{fig}"
        );
        assert_eq!(
            reverse_words("\\section{Introduction}"),
            "\\section{noitcudortnI}"
        );
        assert_eq!(reverse_words("我们研究清洗。"), "洗清究研们我。");
        // Nothing but letters moved.
        let source = "A \\textbf{mixed} line: \\cite{x} {\\em with} $x_{ij}$ and 3.5\\% more.";
        let strip = |s: &str| s.chars().filter(|c| !c.is_alphabetic()).collect::<String>();
        assert_eq!(strip(&reverse_words(source)), strip(source));
        assert_eq!(reverse("⟦ x"), "x ⟦");
    }

    #[tokio::test(start_paused = true)]
    async fn reverses_and_streams() {
        let mock = MockTranslator::default();
        let req = TranslationRequest {
            source: "abc def".into(),
            ..Default::default()
        };
        let seen = std::sync::Mutex::new(String::new());
        let out = mock
            .translate(&req, &|d| *seen.lock().unwrap() = d.to_owned())
            .await
            .unwrap();
        assert_eq!(out.text, "fed cba");
        assert_eq!(*seen.lock().unwrap(), "fed cba");
        assert_eq!(mock.calls(), 1);
    }
}
