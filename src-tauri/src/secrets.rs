//! API key storage in the OS credential store (macOS Keychain, Windows
//! Credential Manager). Keys are written from the settings form and read
//! only by the providers in Rust; no command ever returns one.

/// Keychain service name for all BiWrite secrets.
pub const SERVICE: &str = "app.biwrite.desktop";

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, String>;
    fn set(&self, account: &str, secret: &str) -> Result<(), String>;
    fn delete(&self, account: &str) -> Result<(), String>;
}

/// The platform credential store. Calls may block (and may show an OS
/// prompt), so call them off the async runtime.
pub struct Keychain;

impl Keychain {
    fn entry(account: &str) -> Result<keyring::Entry, String> {
        keyring::Entry::new(SERVICE, account).map_err(|e| e.to_string())
    }
}

impl SecretStore for Keychain {
    fn get(&self, account: &str) -> Result<Option<String>, String> {
        match Self::entry(account)?.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), String> {
        Self::entry(account)?
            .set_password(secret)
            .map_err(|e| e.to_string())
    }

    fn delete(&self, account: &str) -> Result<(), String> {
        match Self::entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// Most keys one provider may hold.
pub const MAX_KEYS: usize = 64;

/// Longest text stored in one credential. Windows Credential Manager holds
/// at most 2560 bytes per credential, stored as UTF-16, so 1280 characters;
/// keys are ASCII. A longer pool is split over several credentials. Other
/// stores take a whole pool, and on macOS each extra item would be one more
/// keychain prompt after an update.
#[cfg(windows)]
const PART_CHARS: usize = 1200;
#[cfg(not(windows))]
const PART_CHARS: usize = usize::MAX;

/// Most parts a pool can have (each holds at least one key).
const MAX_PARTS: usize = MAX_KEYS;

/// Longest key accepted.
const MAX_KEY_CHARS: usize = 1024;

/// Keychain account of part `part` of a provider's additional keys. The
/// first key stays under the provider id itself, which is all that versions
/// without key pools read, so they keep working with that key. The first
/// part is `{id}#pool`, which versions without split pools read.
fn pool_account(id: &str, part: usize) -> String {
    match part {
        0 => format!("{id}#pool"),
        n => format!("{id}#pool{}", n + 1),
    }
}

/// A pasted key with the name written before it, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedKey {
    pub name: Option<String>,
    pub key: String,
}

/// Longest key name kept.
const MAX_NAME_CHARS: usize = 40;

/// Changes between digits and letters, and from a small letter to a
/// capital: frequent in random keys, rare in names (`account2024` has one).
fn transitions(token: &str) -> usize {
    token
        .as_bytes()
        .windows(2)
        .filter(|w| {
            let (a, b) = (w[0], w[1]);
            a.is_ascii_alphanumeric()
                && b.is_ascii_alphanumeric()
                && (a.is_ascii_digit() != b.is_ascii_digit()
                    || (a.is_ascii_lowercase() && b.is_ascii_uppercase()))
        })
        .count()
}

/// Prefixes of keys whose rest may look like anything.
/// Prefixes of keys whose rest may look like anything, with the shortest
/// key length accepted.
const KEY_PREFIXES: &[(&str, usize)] = &[
    ("sk-", 16),
    ("sk_", 16),
    ("bce-v3/", 16),
    ("AIza", 30),
    ("eyJ", 30),
    ("hf_", 30),
    ("gsk_", 30),
    ("xai-", 30),
    ("pplx-", 30),
];

/// Looks like an API key rather than a name: key characters only (letters,
/// digits, `-_.~+/`, `=` padding at the end), and either a known prefix or
/// long, with digits, random-looking (see [`transitions`]) and not words
/// joined into an identifier (see [`is_identifier`]).
fn is_key_token(token: &str) -> bool {
    let body = token.trim_end_matches('=');
    let key_chars = !body.is_empty()
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '/'));
    if !key_chars || token.len() > MAX_KEY_CHARS {
        return false;
    }
    if let Some(&(_, shortest)) = KEY_PREFIXES.iter().find(|(p, _)| token.starts_with(p)) {
        return token.len() >= shortest;
    }
    token.len() >= 20
        && token.bytes().any(|b| b.is_ascii_digit())
        && transitions(token) >= 4
        && !is_identifier(token)
}

/// A model id or host name rather than a key: `/` with `-` (base64 has one
/// or the other: `Qwen/Qwen3-235B-A22B`), or a word among the parts after
/// the first (`Qwen3-30B-A3B-Instruct-2507`, `x.ngrok-free.app`). The first
/// part may be a key's tag (`pplx-…`).
fn is_identifier(token: &str) -> bool {
    (token.contains('/') && token.contains('-'))
        || token.split(['-', '_', '.']).skip(1).any(is_word)
}

/// Four to twelve letters, not all hexadecimal digits, cased like a word
/// (`instruct`, `Instruct`, `INSTRUCT`).
fn is_word(part: &str) -> bool {
    let letters = (4..=12).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphabetic());
    let hex = part.bytes().all(|b| b.is_ascii_hexdigit());
    let rest = part.get(1..).unwrap_or("");
    let cased = rest.bytes().all(|b| b.is_ascii_lowercase())
        || part.bytes().all(|b| b.is_ascii_uppercase());
    letters && !hex && cased
}

/// Could be a secret, although not taken for a key: such text refuses the
/// paste, so the user knows (names are stored in the settings file).
fn looks_secret(word: &str) -> bool {
    word.chars().count() >= 16
        && word.bytes().any(|b| b.is_ascii_digit())
        && transitions(word) >= 4
        && !is_identifier(word)
}

/// A character that may be part of a key.
fn is_key_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '+' | '/' | '=')
}

/// Text that is plainly a name, to be kept in the settings file: every run
/// of key characters in it (non-ASCII text, spaces and punctuation between
/// them) is short, or simple parts joined by `-_.~+/=` (each up to twelve
/// characters with at most two changes between letters and digits:
/// `my-relay-account-2`, `GPT4o-mini-2024`). Anything else may be a key:
/// it is not kept as a name.
pub fn plausible_name(name: &str) -> bool {
    name.split(|c: char| !is_key_char(c))
        .filter(|run| !run.is_empty())
        .all(|run| {
            run.len() <= 15
                || run
                    .split(['-', '_', '.', '~', '+', '/', '='])
                    .all(|part| part.len() <= 12 && transitions(part) <= 2)
        })
}

/// A key name that may hold a secret: it is not plainly a name, or one of
/// its runs of key characters looks like a key.
pub fn may_be_secret(name: &str) -> bool {
    !plausible_name(name)
        || name
            .split(|c: char| !is_key_char(c) || c == '=')
            .any(|w| is_key_token(w) || looks_secret(w))
}

/// The name to store for a key, from what the user typed: one line, not
/// starting with `#` (an exported pool reads that as a comment), at most
/// [`MAX_NAME_CHARS`] characters, judged as stored. Refused if it may hold
/// a key.
pub fn name_to_store(name: &str) -> Result<String, String> {
    let line: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let name: String = line
        .trim()
        .trim_start_matches('#')
        .trim()
        .chars()
        .take(MAX_NAME_CHARS)
        .collect();
    if may_be_secret(&name) {
        return Err(
            "names are stored in the settings file, and this one could be a key: \
                    use plain words (spaces are fine)"
                .into(),
        );
    }
    Ok(name)
}

/// The tokens of a line: split at spaces, `,` and `;`, at any non-ASCII
/// character that is not a letter or digit (full-width punctuation, curly
/// quotes, brackets, invisible characters such as a byte-order mark), and
/// where ASCII text meets other text (`主账号sk-…`).
fn tokens_of(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start: Option<(usize, bool)> = None;
    for (i, c) in line.char_indices() {
        let separator =
            c.is_whitespace() || matches!(c, ',' | ';') || (!c.is_ascii() && !c.is_alphanumeric());
        match start {
            _ if separator => {
                if let Some((s, _)) = start.take() {
                    out.push(&line[s..i]);
                }
            }
            Some((s, ascii)) if ascii != c.is_ascii() => {
                out.push(&line[s..i]);
                start = Some((i, c.is_ascii()));
            }
            Some(_) => {}
            None => start = Some((i, c.is_ascii())),
        }
    }
    if let Some((s, _)) = start {
        out.push(&line[s..]);
    }
    out
}

/// Quotes and brackets around a token (`"sk-…"`, `` `sk-…` ``, `{"api_key":`).
const WRAPPERS: &[char] = &['"', '\'', '`', '(', ')', '[', ']', '{', '}', '<', '>'];

fn bare(token: &str) -> &str {
    token.trim_matches(WRAPPERS)
}

/// A web address: never a key or a name.
fn is_address(token: &str) -> bool {
    token.contains("://") || (token.contains('.') && token.contains('/'))
}

/// Split pasted text into keys with optional names. Accepted shapes, mixed
/// freely: a key per line; a name on one line and its key on the next;
/// `name key`, `name: key`, `name,key` or `name=key` on one line. Keys
/// may also be separated by spaces or commas. Duplicates are dropped (the
/// first name wins) and the order is kept. Text that might be a key but is
/// not recognised as one is refused rather than kept as a name.
pub fn parse_named_keys(text: &str) -> Result<Vec<NamedKey>, String> {
    let mut out: Vec<NamedKey> = Vec::new();
    let mut pending_name: Option<String> = None;
    for line in text.lines() {
        // Comments (the header of an exported pool).
        if line.trim_start().starts_with('#') {
            continue;
        }
        let tokens = tokens_of(line)
            .into_iter()
            .map(bare)
            .filter(|t| t.chars().any(char::is_alphanumeric));
        let mut words: Vec<&str> = Vec::new();
        for token in tokens {
            if token.chars().any(char::is_control) {
                return Err("that does not look like an API key".into());
            }
            // A URL (the provider's page, `BASE_URL=https://…`) is neither.
            if is_address(token) {
                continue;
            }
            // `name:key` or `name=key` written without a space, the key
            // perhaps quoted (`OPENAI_API_KEY="sk-…"`).
            let (word, key) = if is_key_token(token) {
                ("", Some(token))
            } else {
                match token.split_once([':', '=']) {
                    Some((name, key)) if is_key_token(bare(key)) => (bare(name), Some(bare(key))),
                    _ => (token, None),
                }
            };
            let word = bare(word.trim_end_matches([':', '=']));
            if !word.is_empty() {
                if looks_secret(word) {
                    return Err(
                        "some of the text looks like a key BiWrite does not recognise: \
                                put each key on a line of its own, with its name (if any) on \
                                the line before"
                            .into(),
                    );
                }
                words.push(word);
            }
            if let Some(key) = key {
                // A name on the line before belongs to the next key only.
                let before = pending_name.take();
                let name = if words.is_empty() {
                    before
                } else {
                    Some(words.join(" "))
                };
                words.clear();
                push_key(&mut out, name, key);
            }
        }
        if !words.is_empty() {
            pending_name = Some(words.join(" "));
        }
    }
    if out.is_empty() {
        // One token of key characters and nothing else: what else could it be.
        let all: Vec<&str> = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .flat_map(tokens_of)
            .map(bare)
            .collect();
        if let [only] = all[..]
            && only.len() >= 16
            && only.len() <= MAX_KEY_CHARS
            && only.chars().all(is_key_char)
        {
            push_key(&mut out, None, only);
        }
    }
    if out.is_empty() {
        return Err("no API key found in the text".into());
    }
    if out.len() > MAX_KEYS {
        return Err(format!("at most {MAX_KEYS} keys per provider"));
    }
    Ok(out)
}

/// Add `key` with `name` unless the key is there already.
fn push_key(out: &mut Vec<NamedKey>, name: Option<String>, key: &str) {
    if out.iter().any(|k| k.key == key) {
        return;
    }
    // A name that may be a key is dropped, never stored (judged as it
    // would be stored).
    let name = name
        .map(|n| n.chars().take(MAX_NAME_CHARS).collect::<String>())
        .filter(|n| !n.trim().is_empty() && plausible_name(n));
    out.push(NamedKey {
        name,
        key: key.to_owned(),
    });
}

/// Split pasted text into keys, ignoring names (see [`parse_named_keys`]).
#[cfg(test)]
pub fn parse_keys(text: &str) -> Result<Vec<String>, String> {
    Ok(parse_named_keys(text)?.into_iter().map(|k| k.key).collect())
}

/// The stored parts of provider `id`'s additional keys, in order, at most
/// `limit` of them.
fn read_parts(store: &dyn SecretStore, id: &str, limit: usize) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    while parts.len() < limit.min(MAX_PARTS) {
        match store.get(&pool_account(id, parts.len()))? {
            Some(text) => parts.push(text),
            None => break,
        }
    }
    Ok(parts)
}

/// How much of a provider's stored keys the settings vouch for (see
/// `ProviderEntry::vouched`). An older version may have replaced or deleted
/// keys, rewritten only the first part or moved the provider to another
/// host, and left the rest behind: it is never read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vouched {
    /// Keys in all, the first one included: reading stops once there are
    /// as many.
    pub keys: usize,
    /// Parts after the first key, at most.
    pub parts: usize,
}

impl Vouched {
    /// The first key only.
    pub const FIRST: Self = Self { keys: 1, parts: 0 };

    /// `keys` keys, in as many parts as they take (settings saved by a
    /// version that does not record the parts).
    pub fn counted(keys: usize) -> Self {
        Self {
            keys,
            parts: MAX_PARTS,
        }
    }
}

/// Add the keys of a part (one per line), skipping duplicates.
fn push_part(keys: &mut Vec<String>, part: &str) {
    for key in part.lines().map(str::trim).filter(|k| !k.is_empty()) {
        if !keys.iter().any(|k| k == key) {
            keys.push(key.to_owned());
        }
    }
}

/// The keys of provider `id` the settings vouch for, in pool order
/// (blocking). Parts are read only as far as needed.
pub fn get_keys(
    store: &dyn SecretStore,
    id: &str,
    vouched: Vouched,
) -> Result<Vec<String>, String> {
    let mut keys: Vec<String> = store.get(id)?.into_iter().collect();
    if keys.is_empty() {
        return Ok(keys);
    }
    for part in 0..vouched.parts.min(MAX_PARTS) {
        if keys.len() >= vouched.keys {
            break;
        }
        match store.get(&pool_account(id, part))? {
            Some(text) => push_part(&mut keys, &text),
            None => break,
        }
    }
    keys.truncate(vouched.keys);
    Ok(keys)
}

/// Group keys into parts of at most `part_chars` characters, one key per
/// line.
fn into_parts(keys: &[String], part_chars: usize) -> Result<Vec<String>, String> {
    let mut parts: Vec<String> = Vec::new();
    for key in keys {
        if key.chars().count() > part_chars.min(MAX_KEY_CHARS) {
            return Err("that key is too long to store".into());
        }
        match parts.last_mut() {
            Some(part) if part.chars().count() + 1 + key.chars().count() <= part_chars => {
                part.push('\n');
                part.push_str(key);
            }
            _ => parts.push(key.clone()),
        }
    }
    Ok(parts)
}

/// What is stored for a provider: the first key and the parts, all of
/// them (read to change them, and to put them back if the change fails).
pub struct Stored {
    first: Option<String>,
    parts: Vec<String>,
}

impl Stored {
    /// The keys the settings vouch for, in pool order (as [`get_keys`]).
    pub fn keys(&self, vouched: Vouched) -> Vec<String> {
        let mut keys: Vec<String> = self.first.iter().cloned().collect();
        if keys.is_empty() {
            return keys;
        }
        for part in self.parts.iter().take(vouched.parts) {
            if keys.len() >= vouched.keys {
                break;
            }
            push_part(&mut keys, part);
        }
        keys.truncate(vouched.keys);
        keys
    }
}

/// Everything stored for provider `id` (blocking).
pub fn read_stored(store: &dyn SecretStore, id: &str) -> Result<Stored, String> {
    Ok(Stored {
        first: store.get(id)?,
        parts: read_parts(store, id, MAX_PARTS)?,
    })
}

/// Make the store hold `stored` for provider `id`: the parts first, then
/// the first key. Parts after the new ones are deleted up to `stale` (how
/// far older parts may reach), without reading them.
fn write_stored(
    store: &dyn SecretStore,
    id: &str,
    stored: &Stored,
    stale: usize,
) -> Result<(), String> {
    for (i, part) in stored.parts.iter().enumerate() {
        store.set(&pool_account(id, i), part)?;
    }
    // Parts after the new ones are never read (the settings record how
    // many there are), so failing to delete one does not stop the write.
    for extra in stored.parts.len()..stale.min(MAX_PARTS) {
        if let Err(e) = store.delete(&pool_account(id, extra)) {
            log::warn!("could not delete an old part of the keys of {id}: {e}");
        }
    }
    match &stored.first {
        Some(first) => store.set(id, first),
        None => store.delete(id),
    }
}

/// Store exactly `keys` for provider `id` (blocking), in parts of at most
/// `part_chars` characters. Returns the number of parts. `before` is what
/// the caller read ([`read_stored`]): if the write fails, it is put back,
/// so the provider is not left with half of each. Without it nothing is
/// read (each read may be a keychain prompt) and nothing can be put back.
fn set_keys_in_parts(
    store: &dyn SecretStore,
    id: &str,
    keys: &[String],
    part_chars: usize,
    before: Option<&Stored>,
) -> Result<usize, String> {
    let Some((first, rest)) = keys.split_first() else {
        delete_keys(store, id)?;
        return Ok(0);
    };
    let stored = Stored {
        first: Some(first.clone()),
        parts: into_parts(rest, part_chars)?,
    };
    let stale = before.map_or(MAX_PARTS, |b| b.parts.len());
    write_stored(store, id, &stored, stale).inspect_err(|_| {
        let Some(before) = before else {
            return;
        };
        let stale = stored.parts.len().max(before.parts.len());
        if let Err(e) = write_stored(store, id, before, stale) {
            log::error!("could not restore the keys of {id} after a failed write: {e}");
        }
    })?;
    Ok(stored.parts.len())
}

/// Store exactly `keys` for provider `id` (blocking), `before` being what
/// the caller read, if anything (see [`set_keys_in_parts`]). Returns the
/// number of parts the keys after the first take (for
/// `ProviderEntry::key_parts`). An empty list deletes them.
pub fn set_keys(
    store: &dyn SecretStore,
    id: &str,
    keys: &[String],
    before: Option<&Stored>,
) -> Result<usize, String> {
    set_keys_in_parts(store, id, keys, PART_CHARS, before)
}

/// Delete every key of provider `id` (blocking), without reading them: an
/// item that cannot be read can still be deleted. Every item is tried; the
/// first error is returned.
pub fn delete_keys(store: &dyn SecretStore, id: &str) -> Result<(), String> {
    let accounts = (0..MAX_PARTS)
        .map(|part| pool_account(id, part))
        .chain([id.to_owned()]);
    let mut first_error = None;
    for account in accounts {
        if let Err(e) = store.delete(&account) {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

#[cfg(test)]
pub use memory::MemoryStore;

#[cfg(test)]
mod pool_tests {
    use super::*;

    const A: &str = "sk-aaaaaaaaaaaaaaaaaaaaaaaa1";
    const B: &str = "sk-bbbbbbbbbbbbbbbbbbbbbbbb2";
    const C: &str = "sk-cccccccccccccccccccccccc3";

    /// Up to `n` parts, however many keys they hold.
    fn parts(n: usize) -> Vouched {
        Vouched {
            keys: usize::MAX,
            parts: n,
        }
    }

    #[test]
    fn pasted_keys_are_split_and_deduplicated() {
        assert_eq!(
            parse_keys(&format!(" {A}\n{B}, {A}\r\n\"{C}\"  ")).unwrap(),
            [A, B, C]
        );
        assert!(parse_keys(" \n ").is_err());
        assert!(parse_keys("just some words").is_err());
        assert!(parse_keys(&format!("{A}\u{7}")).is_err());
    }

    #[test]
    fn names_come_from_the_line_before_or_the_same_line() {
        let text = format!("main\n{A}\nlab: {B}\n{C}\n");
        let keys = parse_named_keys(&text).unwrap();
        let named: Vec<(Option<&str>, &str)> = keys
            .iter()
            .map(|k| (k.name.as_deref(), k.key.as_str()))
            .collect();
        assert_eq!(named, [(Some("main"), A), (Some("lab"), B), (None, C)]);
        let one_line = parse_named_keys(&format!("main account, {A}")).unwrap();
        assert_eq!(one_line[0].name.as_deref(), Some("main account"));
        let digits_in_name = parse_named_keys(&format!("account2024\n{B}")).unwrap();
        assert_eq!(digits_in_name[0].name.as_deref(), Some("account2024"));
        let glued = parse_named_keys(&format!("lab:{A}\nhome={B}")).unwrap();
        assert_eq!(glued[0].name.as_deref(), Some("lab"));
        assert_eq!(glued[1].name.as_deref(), Some("home"));
        assert_eq!((glued[0].key.as_str(), glued[1].key.as_str()), (A, B));
        // A name on its own line goes to the next key only.
        let keys = parse_named_keys(&format!("main\nlab {A}\n{B}")).unwrap();
        assert_eq!(keys[0].name.as_deref(), Some("lab"));
        assert_eq!(keys[1].name, None);
    }

    #[test]
    fn keys_with_base64_characters_are_keys() {
        let b64 = "Zm9vYmFyYmF6/qux+QUUX12345abcDEF==";
        let keys =
            parse_named_keys(&format!("azure: {b64}\nplain=Ab3dE5gH7jK9mN1pQ3sT5vX7z")).unwrap();
        assert_eq!(keys[0].key, b64);
        assert_eq!(keys[0].name.as_deref(), Some("azure"));
        assert_eq!(keys[1].key, "Ab3dE5gH7jK9mN1pQ3sT5vX7z");
        assert_eq!(keys[1].name.as_deref(), Some("plain"));
    }

    #[test]
    fn json_markdown_and_camel_case_names() {
        let text = format!(
            "{{\"api_key\": \"{A}\"}}\n- main: `{B}`\nworkAccountForBiWrite\nrelay.example.com/console/9f8a7b6c5d4e3f2a\n{C}"
        );
        let keys = parse_named_keys(&text).unwrap();
        let named: Vec<(Option<&str>, &str)> = keys
            .iter()
            .map(|k| (k.name.as_deref(), k.key.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                (Some("api_key"), A),
                (Some("main"), B),
                // Not plainly a name (it could be a key): not kept.
                (None, C)
            ]
        );
    }

    #[test]
    fn model_ids_and_host_names_are_not_keys() {
        for text in [
            "OPENAI_MODEL=Qwen/Qwen3-235B-A22B",
            "Qwen3-30B-A3B-Instruct-2507",
            "Mixtral-8x7B-Instruct-v0.1",
            "deepseek-ai/DeepSeek-R1-0528-Qwen3-8B",
            "3f1a-2a02-8109-b6c0.ngrok-free.app",
        ] {
            let keys = parse_named_keys(&format!("{text}\n{A}")).unwrap();
            assert_eq!(keys.len(), 1, "{text}");
            assert_eq!(keys[0].key, A);
        }
    }

    #[test]
    fn real_key_shapes_are_keys() {
        for key in [
            "sk-proj-Ab3dE5gH7jK9mN1pQ3sT5vX7zAb3dE5gH7jK9",
            "sk-ant-api03-Ab3dE5gH7jK9mN1pQ3sT5vX7z-Ab3dE5gH7jK9mN1pQ3s",
            "sk-or-v1-3f9a2c8b1d4e6f703f9a2c8b1d4e6f703f9a2c8b1d4e6f70",
            "pplx-3f9aQ2c8bW1d4eR6f70T3f9aY2c8bU1d4eI6f70O3f9a",
            "gsk_Ab3dE5gH7jK9mN1pQ3sT5vX7zAb3dE5gH7jK9mN1pQ3s",
            "AIzaFakeD0hCZtE6vySjMm-WEfRq3CPzq",
            "3f9a2c8b1d4e6f703f9a2c8b1d4e6f70",
            "3f9a2c8b1d4e6f703f9a2c8b1d4e6f70.Ab3dE5gH7jK9mN1p",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.Ab3dE5gH7jK9mN1pQ3sT5vX7z",
            "bce-v3/ALTAK-Ab3dE5gH7jK9mN1pQ3sT/3f9a2c8b1d4e6f703f9a2c8b1d4e6f70",
            "Zm9vYmFyYmF6/qux+QUUX12345abcDEF==",
        ] {
            assert_eq!(parse_keys(key).unwrap(), [key], "{key}");
        }
    }

    /// A little deterministic generator (no dependency).
    fn random_tokens(alphabet: &[u8], len: usize, count: usize) -> Vec<String> {
        let mut state: u64 = 0x9E37_79B9_7F4A_7C15 ^ (len as u64) ^ ((alphabet.len() as u64) << 32);
        (0..count)
            .map(|_| {
                (0..len)
                    .map(|_| {
                        state ^= state << 13;
                        state ^= state >> 7;
                        state ^= state << 17;
                        char::from(alphabet[(state % alphabet.len() as u64) as usize])
                    })
                    .collect()
            })
            .collect()
    }

    #[test]
    fn keys_in_chinese_or_typographic_surroundings_are_keys() {
        const P: &str = "sk-proj-Ab3dE5gH7jK9mN1pQ3sT5vX7zAb3dE5";
        for text in [
            format!("“{P}”\n{A}"),
            format!("主账号：{P}\n{A}"),
            format!("{P}。\n{A}"),
            format!("{P}\u{200b}\n{A}"),
            format!("【主力】{P}\n{A}"),
            format!("「{P}」\n{A}"),
            format!("\u{feff}{P}\n{A}"),
            format!("主账号{P}\n{A}"),
        ] {
            let keys = parse_named_keys(&text).unwrap();
            let found: Vec<&str> = keys.iter().map(|k| k.key.as_str()).collect();
            assert_eq!(found, [P, A], "{text}");
            assert!(
                keys.iter()
                    .all(|k| k.name.as_deref().is_none_or(|n| !n.contains("sk-"))),
                "{text}"
            );
        }
        let keys = parse_named_keys(&format!("主账号：\n{P}")).unwrap();
        assert_eq!(keys[0].name.as_deref(), Some("主账号"));
    }

    #[test]
    fn prefixed_keys_and_a_lone_key_are_keys() {
        for key in [
            "AIza0Hy9FKi85HZr5x-JUBRS_2wCgLAo0",
            "AIzaFakeIhCZtEvySjMmWEfRqCPzqKqqs",
            "hf_FakeTestTokenForTheParserOnlyX",
            "eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiJhYmMifQ.Ab3d-Instruct-E5gH7jK9mN1pQ3sT",
        ] {
            // Among other keys (alone, any token of key characters is one).
            assert_eq!(
                parse_keys(&format!("{key}\n{A}")).unwrap(),
                [key, A],
                "{key}"
            );
        }
        // A single key of an unknown shape, pasted alone.
        assert_eq!(
            parse_keys("abcdefghijklmnopqrstuvwx").unwrap(),
            ["abcdefghijklmnopqrstuvwx"]
        );
        assert!(parse_keys("abcdefgh").is_err());
    }

    #[test]
    fn random_keys_never_become_names() {
        let letters = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
        let lower_digits = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let base62 = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
        for alphabet in [&letters[..], &lower_digits[..], &base62[..]] {
            for len in [16, 20, 24, 28] {
                for token in random_tokens(alphabet, len, 500) {
                    // On the line before a key, on its line, and in Chinese
                    // or typographic surroundings.
                    for text in [
                        format!("{token}\n{A}"),
                        format!("{token} {A}"),
                        format!("“{token}”\n{A}"),
                        format!("主账号：{token}\n{A}"),
                        format!("主账号{token}\n{A}"),
                        format!("\u{feff}{token}\n{A}"),
                    ] {
                        if let Ok(keys) = parse_named_keys(&text) {
                            assert!(
                                keys.iter().all(|k| k
                                    .name
                                    .as_deref()
                                    .is_none_or(|n| !n.contains(&*token))),
                                "{token} kept as a name"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn renamed_keys_get_one_line_names_that_cannot_be_keys() {
        assert_eq!(name_to_store("  lab\naccount ").unwrap(), "lab account");
        assert_eq!(name_to_store("# main").unwrap(), "main");
        assert_eq!(name_to_store("").unwrap(), "");
        for name in [
            "“sk-proj-Ab3dE5gH7jK9mN1pQ3sT5vX7z”",
            "“sk-relay-Ab3dE5gH7jK9mN1pQ3sT”",
            "hf_FakeTestTokenForTheParserOnlyX",
            "主账号：sk-3f9a2c8b1d4e6f703f9a2c8b",
        ] {
            assert!(name_to_store(name).is_err(), "{name}");
        }
        // Judged after cutting to the stored length.
        let long = format!("{}{}", "实".repeat(30), "sk-proj-Ab3dE5gH7jK9mN1pQ3sT5vX7z");
        assert!(name_to_store(&long).is_ok_and(|n| !n.contains("sk-proj-Ab3dE5gH7jK9mN1p")));
    }

    #[test]
    fn plain_names_are_kept_and_others_dropped() {
        for name in [
            "main",
            "lab account",
            "account2024",
            "my-relay-account-number-1",
            "production_account_2024",
            "someone.name@university.edu",
            "实验室账号二号备用",
            "GPT4oMini2024",
            "zhangsan1998@163.com",
            "john.doe2@university.edu",
            "GPT4o-mini-2024-07-18",
            "deepseek-chat-v3",
            "Qwen/Qwen3-235B-A22B",
            "主账号 lab-2",
        ] {
            assert!(plausible_name(name), "{name}");
        }
        for name in [
            "workAccountForBiWrite",
            "GOUWVzgsoCMnaINuQSAj",
            "“sk-proj-Ab3dE5gH7jK9mN1pQ3sT5vX7z",
            "主账号sk-proj-Ab3dE5gH7jK9mN1pQ3sT",
            "\u{feff}sk-relay-Ab3dE5gH7jK9mN1pQ3sT",
        ] {
            assert!(!plausible_name(name), "{name}");
            assert!(may_be_secret(name), "{name}");
        }
    }

    #[test]
    fn long_names_are_names_not_keys() {
        let keys = parse_named_keys(&format!(
            "my-relay-account-number-1\n{A}\nproduction_account_2024 {B}"
        ))
        .unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(keys[0].name.as_deref(), Some("my-relay-account-number-1"));
        assert_eq!(keys[1].name.as_deref(), Some("production_account_2024"));
    }

    #[test]
    fn text_that_may_be_a_secret_is_never_kept_as_a_name() {
        // Too short to be taken for a key, too random to be a name.
        for text in [
            format!("x7Kq2Lm9Pz4R8sT1\n{A}"),
            format!("a3F9k2L8q1W5e7R0Y {A}"),
            format!("x7Kq2Lm9Pz4R8sT1$!\n{A}"),
        ] {
            let err = parse_named_keys(&text).unwrap_err();
            assert!(err.contains("does not recognise"), "{text}: {err}");
            assert!(!err.contains("x7Kq"), "the error repeats the text");
        }
        assert!(may_be_secret("lab x7Kq2Lm9Pz4R8sT1"));
        // Random text without digits is not kept as a name either.
        let lettered = "QwErTyUiOpAsDfGhJkLzXcVb";
        let keys = parse_named_keys(&format!("{lettered}\n{A}")).unwrap();
        assert_eq!((keys.len(), keys[0].name.as_deref()), (1, None));
        assert!(may_be_secret("QwErTyUiOpAsDfGh"));
        assert!(may_be_secret(A));
        assert!(!may_be_secret("GPT4oMini2024 account"));
    }

    #[test]
    fn env_lines_urls_and_model_names_are_understood() {
        let long_name = "q".repeat(40);
        let text = format!(
            "# from .env\nOPENAI_API_KEY=\"{A}\"\nOPENAI_BASE_URL=https://api.openai.com/v1/chat/completions\n\
             GPT4oMini2024\n(https://relay.example.com/console/9f8a7b6c5d4e3f2a)\n{B}\n{long_name}\n{C}"
        );
        let keys = parse_named_keys(&text).unwrap();
        let named: Vec<(Option<&str>, &str)> = keys
            .iter()
            .map(|k| (k.name.as_deref(), k.key.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                (Some("OPENAI_API_KEY"), A),
                (Some("GPT4oMini2024"), B),
                (None, C)
            ]
        );
    }

    #[test]
    fn the_first_key_stays_where_older_versions_look() {
        let store = MemoryStore::default();
        let keys: Vec<String> = [A, B, C].map(String::from).to_vec();
        set_keys(&store, "p-1", &keys, None).unwrap();
        assert_eq!(store.get("p-1").unwrap().as_deref(), Some(A));
        assert_eq!(get_keys(&store, "p-1", parts(1)).unwrap(), keys);

        set_keys(&store, "p-1", &keys[1..2], None).unwrap();
        assert_eq!(get_keys(&store, "p-1", parts(1)).unwrap(), [B]);
        assert_eq!(store.get("p-1#pool").unwrap(), None);

        // A key stored by an older version is a pool of one.
        store.set("p-2", "sk-old").unwrap();
        assert_eq!(get_keys(&store, "p-2", parts(1)).unwrap(), ["sk-old"]);

        delete_keys(&store, "p-1").unwrap();
        assert!(get_keys(&store, "p-1", parts(1)).unwrap().is_empty());
    }

    #[test]
    fn pool_keys_are_read_only_when_vouched_for() {
        let store = MemoryStore::default();
        set_keys(&store, "p-1", &[A, B, C].map(String::from), None).unwrap();
        assert_eq!(get_keys(&store, "p-1", Vouched::FIRST).unwrap(), [A]);
        // A version without pools deleted the first key: the rest is left over.
        store.delete("p-1").unwrap();
        assert!(get_keys(&store, "p-1", parts(1)).unwrap().is_empty());
    }

    fn many_keys(n: usize) -> Vec<String> {
        (0..n)
            .map(|i| format!("sk-relay-{i:03}-{}", "k".repeat(40)))
            .collect()
    }

    /// Parts as on Windows.
    const WINDOWS_PART: usize = 1200;

    #[test]
    fn a_large_pool_is_split_into_credential_sized_parts() {
        let store = MemoryStore::default();
        let keys = many_keys(MAX_KEYS);
        let count = set_keys_in_parts(&store, "p-1", &keys, WINDOWS_PART, None).unwrap();
        assert!(count > 1);
        assert_eq!(get_keys(&store, "p-1", parts(count)).unwrap(), keys);
        let stored = read_parts(&store, "p-1", MAX_PARTS).unwrap();
        assert_eq!(stored.len(), count);
        assert!(stored.iter().all(|p| p.encode_utf16().count() * 2 <= 2560));
        // Fewer keys: the parts no longer needed are deleted.
        assert_eq!(
            set_keys_in_parts(&store, "p-1", &keys[..3], WINDOWS_PART, None).unwrap(),
            1
        );
        assert_eq!(read_parts(&store, "p-1", MAX_PARTS).unwrap().len(), 1);
        assert_eq!(store.get("p-1#pool2").unwrap(), None);
        assert_eq!(get_keys(&store, "p-1", parts(1)).unwrap(), keys[..3]);
        // Elsewhere a pool is one item.
        if cfg!(not(windows)) {
            assert_eq!(set_keys(&store, "p-1", &keys, None).unwrap(), 1);
            assert_eq!(store.get("p-1#pool2").unwrap(), None);
            assert_eq!(get_keys(&store, "p-1", parts(1)).unwrap(), keys);
        }
    }

    #[test]
    fn parts_left_by_a_version_that_reads_one_part_stay_unread() {
        let store = MemoryStore::default();
        let old = many_keys(40);
        let count = set_keys_in_parts(&store, "p-1", &old, WINDOWS_PART, None).unwrap();
        assert!(count >= 2);
        assert_eq!(
            get_keys(
                &store,
                "p-1",
                Vouched {
                    keys: 40,
                    parts: count
                }
            )
            .unwrap(),
            old
        );
        // 0.2.x replaces the keys: it writes the first key and `#pool` only,
        // records 2 keys and drops `keyParts`.
        store.set("p-1", A).unwrap();
        store.set("p-1#pool", B).unwrap();
        assert!(store.get("p-1#pool2").unwrap().is_some(), "left over");
        assert_eq!(
            get_keys(&store, "p-1", Vouched::counted(2)).unwrap(),
            [A, B]
        );
        let stored = read_stored(&store, "p-1").unwrap();
        assert_eq!(stored.keys(Vouched::counted(2)), [A, B]);
    }

    #[test]
    fn parts_are_found_by_count_when_older_settings_dropped_their_number() {
        // Windows: a pool in two parts; 0.2.x saved its settings (dropping
        // `keyParts`) without touching the keys, so `keyCount` still holds.
        let store = MemoryStore::default();
        let keys = many_keys(40);
        assert_eq!(
            set_keys_in_parts(&store, "p-1", &keys, WINDOWS_PART, None).unwrap(),
            2
        );
        assert_eq!(get_keys(&store, "p-1", Vouched::counted(40)).unwrap(), keys);
        assert_eq!(
            read_stored(&store, "p-1")
                .unwrap()
                .keys(Vouched::counted(40)),
            keys
        );
    }

    /// A store whose items cannot be read.
    #[derive(Default)]
    struct Unreadable(MemoryStore);

    impl SecretStore for Unreadable {
        fn get(&self, _account: &str) -> Result<Option<String>, String> {
            Err("the user denied access".into())
        }

        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            self.0.set(account, secret)
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            self.0.delete(account)
        }
    }

    /// Counts reads (each may be a keychain prompt).
    #[derive(Default)]
    struct CountsReads {
        inner: MemoryStore,
        reads: std::sync::atomic::AtomicUsize,
    }

    impl SecretStore for CountsReads {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            self.inner.delete(account)
        }
    }

    #[test]
    fn writing_keys_reads_nothing_more() {
        let store = CountsReads::default();
        let reads = || store.reads.load(std::sync::atomic::Ordering::SeqCst);
        set_keys(&store, "p-1", &[A, B].map(String::from), None).unwrap();
        delete_keys(&store, "p-1").unwrap();
        assert_eq!(reads(), 0);
        // An edit reads once, and the write reuses what was read.
        set_keys(&store, "p-1", &[A, B].map(String::from), None).unwrap();
        let stored = read_stored(&store, "p-1").unwrap();
        let after_read = reads();
        let mut keys = stored.keys(Vouched::counted(2));
        keys.push(C.to_owned());
        set_keys(&store, "p-1", &keys, Some(&stored)).unwrap();
        assert_eq!(reads(), after_read);
    }

    /// A store that cannot delete one account.
    struct KeepsOne {
        inner: MemoryStore,
        account: &'static str,
    }

    impl SecretStore for KeepsOne {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            if account == self.account {
                return Err("locked".into());
            }
            self.inner.delete(account)
        }
    }

    #[test]
    fn an_old_part_that_cannot_be_deleted_does_not_stop_the_write() {
        let store = KeepsOne {
            inner: MemoryStore::default(),
            account: "p-1#pool2",
        };
        let old = many_keys(40);
        set_keys_in_parts(&store, "p-1", &old, WINDOWS_PART, None).unwrap();
        let new = [A, B].map(String::from);
        let parts = set_keys_in_parts(&store, "p-1", &new, WINDOWS_PART, None).unwrap();
        assert_eq!(store.get("p-1").unwrap().as_deref(), Some(A));
        assert!(
            store.get("p-1#pool2").unwrap().is_some(),
            "left, but not read"
        );
        assert_eq!(
            get_keys(&store, "p-1", Vouched { keys: 2, parts }).unwrap(),
            new
        );
    }

    #[test]
    fn no_more_keys_than_vouched_for_are_read() {
        let store = MemoryStore::default();
        store.set("p-1", A).unwrap();
        // The first key written, the rest of an older pool still there.
        store.set("p-1#pool", &format!("{B}\n{C}")).unwrap();
        assert_eq!(
            get_keys(&store, "p-1", Vouched::counted(2)).unwrap(),
            [A, B]
        );
        assert_eq!(
            read_stored(&store, "p-1")
                .unwrap()
                .keys(Vouched::counted(2)),
            [A, B]
        );
    }

    #[test]
    fn keys_that_cannot_be_read_can_still_be_replaced_or_deleted() {
        let store = Unreadable::default();
        store.0.set("p-1", A).unwrap();
        store.0.set("p-1#pool", B).unwrap();
        store.0.set("p-1#pool7", C).unwrap();
        set_keys(&store, "p-1", &[C.to_owned()], None).unwrap();
        assert_eq!(store.0.get("p-1").unwrap().as_deref(), Some(C));
        assert_eq!(store.0.get("p-1#pool").unwrap(), None);
        assert_eq!(store.0.get("p-1#pool7").unwrap(), None);
        store.0.set("p-1#pool", B).unwrap();
        delete_keys(&store, "p-1").unwrap();
        assert_eq!(store.0.get("p-1").unwrap(), None);
        assert_eq!(store.0.get("p-1#pool").unwrap(), None);
    }

    /// A store that refuses to write one account.
    struct Refuses {
        inner: MemoryStore,
        account: &'static str,
    }

    impl SecretStore for Refuses {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            self.inner.get(account)
        }

        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            if account == self.account {
                return Err("too large".into());
            }
            self.inner.set(account, secret)
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            self.inner.delete(account)
        }
    }

    #[test]
    fn a_failed_write_leaves_the_old_keys() {
        let store = Refuses {
            inner: MemoryStore::default(),
            account: "p-1#pool2",
        };
        let old: Vec<String> = [A, B].map(String::from).to_vec();
        set_keys_in_parts(&store, "p-1", &old, WINDOWS_PART, None).unwrap();
        let mut new = vec![C.to_owned()];
        new.extend(many_keys(40));
        let before = read_stored(&store, "p-1").unwrap();
        assert!(set_keys_in_parts(&store, "p-1", &new, WINDOWS_PART, Some(&before)).is_err());
        assert_eq!(get_keys(&store, "p-1", parts(MAX_PARTS)).unwrap(), old);
        // The first key is written last.
        let store = Refuses {
            inner: MemoryStore::default(),
            account: "p-2",
        };
        let before = read_stored(&store, "p-2").unwrap();
        assert!(set_keys_in_parts(&store, "p-2", &new, WINDOWS_PART, Some(&before)).is_err());
        assert!(get_keys(&store, "p-2", parts(1)).unwrap().is_empty());
        assert_eq!(store.get("p-2#pool").unwrap(), None);
    }
}

#[cfg(test)]
mod memory {
    use std::collections::HashMap;
    use std::sync::{Mutex, PoisonError};

    use super::SecretStore;

    /// In-memory store (tests).
    #[derive(Default)]
    pub struct MemoryStore {
        map: Mutex<HashMap<String, String>>,
    }

    impl SecretStore for MemoryStore {
        fn get(&self, account: &str) -> Result<Option<String>, String> {
            Ok(self
                .map
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(account)
                .cloned())
        }

        fn set(&self, account: &str, secret: &str) -> Result<(), String> {
            self.map
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(account.to_owned(), secret.to_owned());
            Ok(())
        }

        fn delete(&self, account: &str) -> Result<(), String> {
            self.map
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(account);
            Ok(())
        }
    }
}
