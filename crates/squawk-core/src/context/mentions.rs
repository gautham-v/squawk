//! Spoken file references → `@path` mentions.
//!
//! What people say, and what the model writes down:
//! - "audio dot rs", "audio.rs", "Audio.rs"          → `@src/audio.rs`
//! - "cargo toml", "cargo dot toml", "Cargo.toml"    → `@Cargo.toml`
//! - "the claude md" / "claude dot md"               → `@CLAUDE.md`
//! - "readme", "the read me"                         → `@README.md`
//! - "source slash main dot rs", "src/main.rs"       → `@src/main.rs`
//! - "status item dot rs"                            → `@src/status_item.rs`
//! - "at sign audio.rs", "the audio dot rs file"     → `@src/audio.rs`
//!
//! A reference resolves when exactly one file matches, or when exactly one of
//! the matches is shallowest ("cargo toml" in a workspace is the root one;
//! two `mod.rs` at the same depth stay as spoken). Spoken directories narrow
//! the match ("context slash mod dot rs"). Anything less certain is left
//! alone: a wrong @mention costs more than a missing one.

use super::index::{squash, Index, STEM_ALIASES};
use crate::text::Chunk;

/// Longest run of chunks one reference can span ("crates slash squawk core
/// slash src slash context slash mod dot rs" is 12).
const MAX_SPAN: usize = 24;

/// Extensions said without "dot" only count when they are not English.
const EXT_WORD_STOP: &[&str] = &[
    "a", "am", "an", "as", "at", "be", "by", "do", "go", "he", "hi", "i", "if", "in", "is", "it",
    "me", "my", "no", "of", "ok", "on", "or", "so", "to", "up", "us", "we", "old", "new", "bak",
    "out", "log", "tmp", "orig",
];

/// Model spellings of extensions ("cargo tomel" is how Parakeet often
/// writes "cargo toml").
const EXT_ALIASES: &[(&str, &str)] = &[
    ("jason", "json"),
    ("markdown", "md"),
    ("yammel", "yaml"),
    ("yamel", "yaml"),
    ("tommel", "toml"),
    ("tomal", "toml"),
    ("tomel", "toml"),
    ("tomle", "toml"),
];

/// The extension a spoken word stands for.
fn ext_alias(word: &str) -> &str {
    EXT_ALIASES
        .iter()
        .find(|(from, _)| *from == word)
        .map_or(word, |(_, to)| to)
}

/// Dotfiles people name without the dot ("the gitignore").
const DOTLESS: &[&str] = &[
    "gitignore",
    "gitattributes",
    "gitmodules",
    "dockerignore",
    "editorconfig",
    "prettierrc",
    "eslintrc",
    "npmrc",
    "nvmrc",
    "envrc",
];

/// Words after which a bare "at" means the @ sign rather than "look at".
const AT_AFTER: &[&str] = &[
    "and", "or", "plus", "also", "then", "mention", "include", "tag", "attach",
];

#[derive(Debug, Clone, PartialEq)]
enum Sym {
    Word(String),
    Dot,
    Slash,
    /// "underscore", "dash": part of a name, adds nothing to the squashed form.
    Join,
}

#[derive(Debug)]
struct Tok {
    syms: Vec<Sym>,
    /// Lowercased core, for "the"/"at"/"file".
    key: String,
    /// Spoken "dot"/"slash"/"underscore": only meaningful inside a reference.
    connector: bool,
    /// A lone "@".
    at_sign: bool,
    /// Can't be part of a reference (URL, existing @mention, punctuation).
    barrier: bool,
    /// The model wrote a file name or path itself (`audio.rs`).
    written: bool,
}

impl Tok {
    /// Carries a dot or a slash: the shape of a file reference.
    fn is_structural(&self) -> bool {
        self.syms.iter().any(|s| matches!(s, Sym::Dot | Sym::Slash))
    }

    fn from_chunk(c: &Chunk) -> Tok {
        let key = c.key();
        let mut tok = Tok {
            syms: Vec::new(),
            key: key.clone(),
            connector: false,
            at_sign: false,
            barrier: false,
            written: false,
        };
        if c.core == "@" {
            tok.at_sign = true;
            return tok;
        }
        let core = c.core.as_str();
        if core.is_empty()
            || core.starts_with(['@', '/', '~'])
            || core.contains(['@', ':', '`'])
            || core.contains("://")
        {
            tok.barrier = true;
            return tok;
        }
        if c.is_protected() {
            // A file name or relative path the model wrote out.
            tok.written = true;
            let mut word = String::new();
            for ch in core.chars() {
                match ch {
                    '.' | '/' => {
                        if !word.is_empty() {
                            tok.syms.push(Sym::Word(std::mem::take(&mut word)));
                        }
                        tok.syms.push(if ch == '.' { Sym::Dot } else { Sym::Slash });
                    }
                    c if c.is_alphanumeric() => word.extend(c.to_lowercase()),
                    _ => {}
                }
            }
            if !word.is_empty() {
                tok.syms.push(Sym::Word(word));
            }
            return tok;
        }
        let sym = match key.as_str() {
            "dot" => Sym::Dot,
            "slash" => Sym::Slash,
            "underscore" | "dash" | "hyphen" => Sym::Join,
            _ => {
                let w = match number_word(&key) {
                    Some(n) => n.to_string(),
                    None => squash(&key),
                };
                if w.is_empty() {
                    tok.barrier = true;
                    return tok;
                }
                Sym::Word(w)
            }
        };
        tok.connector = !matches!(sym, Sym::Word(_));
        tok.syms.push(sym);
        tok
    }
}

/// A spoken word that can be an extension on its own ("toml" in "cargo toml").
fn is_ext_word(tok: &Tok, index: &Index) -> bool {
    match tok.syms.as_slice() {
        [Sym::Word(w)] => {
            !tok.written && index.has_ext(ext_alias(w)) && !EXT_WORD_STOP.contains(&w.as_str())
        }
        _ => false,
    }
}

/// "seven" in "file underscore seven dot rs" is `7` in the file name.
fn number_word(word: &str) -> Option<usize> {
    const SMALL: &str = "zero one two three four five six seven eight nine ten eleven twelve \
                         thirteen fourteen fifteen sixteen seventeen eighteen nineteen twenty";
    SMALL.split_whitespace().position(|w| w == word)
}

/// A parsed reference: directories (squashed) and a name with dots.
#[derive(Debug, PartialEq)]
struct Spoken {
    dirs: Vec<String>,
    /// `audio.rs`, `.gitignore`, `readme`.
    name: String,
    /// The name with its last word read as an extension ("cargo toml" →
    /// `cargo.toml`), when that is plausible.
    name_with_ext: Option<String>,
}

pub(crate) fn apply(chunks: Vec<Chunk>, index: &Index) -> Vec<Chunk> {
    if !index.has_files() {
        return chunks;
    }
    let toks: Vec<Tok> = chunks.iter().map(Tok::from_chunk).collect();
    let n = toks.len();
    let mut out: Vec<Chunk> = Vec::with_capacity(n);
    // Which original chunk each `out` entry is (None: a mention we made).
    let mut origin: Vec<Option<usize>> = Vec::with_capacity(n);
    let mut i = 0;
    while i < n {
        let Some((mut end, path)) = match_at(&toks, &chunks, i, index) else {
            out.push(chunks[i].clone());
            origin.push(Some(i));
            i += 1;
            continue;
        };

        // Words before the reference that belong to it: "the", "at sign", "@".
        let mut lead = chunks[i].lead.clone();
        let consumed = leading_words(&toks, &chunks, i);
        let mut took_the = false;
        // Only words still in `out` as spoken (not part of an earlier mention).
        let still_there =
            (1..=consumed).all(|k| origin.len() >= k && origin[origin.len() - k] == Some(i - k));
        if consumed > 0 && still_there {
            took_the = toks[i - 1].key == "the";
            lead = format!("{}{}", chunks[i - consumed].lead, lead);
            out.truncate(out.len() - consumed);
            origin.truncate(origin.len() - consumed);
        }
        // "the audio dot rs file": the file goes with the the.
        if took_the
            && end < n
            && toks[end].key == "file"
            && chunks[end - 1].trail.is_empty()
            && chunks[end].lead.is_empty()
        {
            end += 1;
        }
        // A sentence-ending period glued to a path reads as part of it.
        let trail: String = chunks[end - 1]
            .trail
            .chars()
            .filter(|c| !matches!(c, '.' | '…'))
            .collect();
        out.push(Chunk {
            lead,
            core: format!("@{path}"),
            trail,
        });
        origin.push(None);
        i = end;
    }
    out
}

/// How many chunks right before `start` are part of the reference.
fn leading_words(toks: &[Tok], chunks: &[Chunk], start: usize) -> usize {
    let quiet = |k: usize| chunks[k].trail.is_empty();
    if start == 0 || !quiet(start - 1) {
        return 0;
    }
    let prev = &toks[start - 1];
    if prev.key == "the" || prev.at_sign {
        return 1;
    }
    if matches!(prev.key.as_str(), "sign" | "symbol")
        && start >= 2
        && toks[start - 2].key == "at"
        && quiet(start - 2)
    {
        return 2;
    }
    if prev.key == "at" {
        // "at audio dot rs" is the @ sign; "look at audio dot rs" is English.
        let at_start = start == 1;
        let after_break = start >= 2 && !chunks[start - 2].trail.is_empty();
        let after_list = start >= 2 && AT_AFTER.contains(&toks[start - 2].key.as_str());
        if at_start || after_break || after_list {
            return 1;
        }
    }
    0
}

/// The longest reference starting at chunk `start`: (end, path).
fn match_at(
    toks: &[Tok],
    chunks: &[Chunk],
    start: usize,
    index: &Index,
) -> Option<(usize, String)> {
    let first = &toks[start];
    if first.barrier || first.at_sign || first.connector && first.syms != [Sym::Dot] {
        return None;
    }
    // Mid-reference: "src slash foo dot rs" must not match from "foo".
    if start > 0 && toks[start - 1].connector && chunks[start - 1].trail.is_empty() {
        return None;
    }
    // The span can't run past a barrier, a chunk with leading punctuation,
    // or one with trailing punctuation (which may end it).
    let mut limit = start;
    for k in start..toks.len().min(start + MAX_SPAN) {
        if toks[k].barrier || toks[k].at_sign || (k > start && !chunks[k].lead.is_empty()) {
            break;
        }
        limit = k + 1;
        if !chunks[k].trail.is_empty() {
            break;
        }
    }
    let first_structural = (start..limit).find(|&k| toks[k].is_structural());
    for end in (start + 1..=limit).rev() {
        let span = &toks[start..end];
        // …and must not stop short of a connector: "foo dot rs dot orig".
        if end < toks.len() && toks[end].connector && chunks[end - 1].trail.is_empty() {
            continue;
        }
        // Cheap reject for plain prose: with no "dot"/"slash" in it, a
        // reference is "cargo toml" (ends in an extension) or a short name
        // ("readme", "make file").
        let structural = first_structural.is_some_and(|k| k < end);
        if !structural && span.len() > 3 && !is_ext_word(&toks[end - 1], index) {
            continue;
        }
        let Some(spoken) = parse(span, index) else {
            continue;
        };
        let after_the =
            start > 0 && toks[start - 1].key == "the" && chunks[start - 1].trail.is_empty();
        if let Some(path) = resolve(&spoken, span, after_the, index) {
            return Some((end, path));
        }
    }
    None
}

/// Tokens → directories and a name, or `None` if they don't read as a path.
fn parse(span: &[Tok], index: &Index) -> Option<Spoken> {
    let syms: Vec<&Sym> = span.iter().flat_map(|t| &t.syms).collect();
    let mut segments: Vec<Vec<&Sym>> = vec![Vec::new()];
    for s in syms {
        if *s == Sym::Slash {
            segments.push(Vec::new());
        } else {
            segments.last_mut().expect("non-empty").push(s);
        }
    }
    let last = segments.pop().expect("non-empty");
    let mut dirs = Vec::with_capacity(segments.len());
    for seg in segments {
        let mut dir = String::new();
        for s in seg {
            match s {
                Sym::Word(w) => dir.push_str(w),
                Sym::Join => {}
                _ => return None,
            }
        }
        if dir.is_empty() {
            return None;
        }
        dirs.push(dir);
    }

    let mut name = String::new();
    let mut words: Vec<&str> = Vec::new();
    let mut prev_dot = false;
    for s in &last {
        match s {
            Sym::Word(w) => {
                name.push_str(w);
                words.push(w);
                prev_dot = false;
            }
            Sym::Dot => {
                if prev_dot {
                    return None;
                }
                name.push('.');
                prev_dot = true;
            }
            Sym::Join => {}
            Sym::Slash => unreachable!("split above"),
        }
    }
    if words.is_empty() || prev_dot {
        return None;
    }
    if let Some((stem, ext)) = name.rsplit_once('.') {
        name = format!("{stem}.{}", ext_alias(ext));
    }

    // "cargo toml": the last spoken word is the extension. Only when it was
    // its own spoken word, not part of something the model wrote.
    let last_tok = span.last().expect("non-empty");
    let name_with_ext = (!name.contains('.')
        && words.len() >= 2
        && !last_tok.written
        && matches!(last_tok.syms.as_slice(), [Sym::Word(_)]))
    .then(|| {
        let ext = *words.last().expect("len >= 2");
        is_ext_word(last_tok, index)
            .then(|| format!("{}.{}", &name[..name.len() - ext.len()], ext_alias(ext)))
    })
    .flatten();
    Some(Spoken {
        dirs,
        name,
        name_with_ext,
    })
}

/// The one file a reference means, relative to the root.
fn resolve(spoken: &Spoken, span: &[Tok], after_the: bool, index: &Index) -> Option<String> {
    let mut candidates: Vec<u32> = Vec::new();
    if let Some(name) = &spoken.name_with_ext {
        candidates = named_in(name, &spoken.dirs, index);
    }
    if candidates.is_empty() {
        candidates = if spoken.name.contains('.') {
            let mut c = named_in(&spoken.name, &spoken.dirs, index);
            if c.is_empty() {
                if let Some(alt) = yaml_twin(&spoken.name) {
                    c = named_in(&alt, &spoken.dirs, index);
                }
            }
            c
        } else if DOTLESS.contains(&spoken.name.as_str()) {
            named_in(&format!(".{}", spoken.name), &spoken.dirs, index)
        } else if !spoken.dirs.is_empty() || Index::is_bare_name(&spoken.name) {
            named_in(&spoken.name, &spoken.dirs, index)
        } else if STEM_ALIASES.contains(&spoken.name.as_str())
            && (after_the || !is_read_me_phrase(span))
        {
            index.by_stem(&spoken.name).to_vec()
        } else {
            Vec::new()
        };
    }
    choose(&candidates, index).map(str::to_string)
}

/// "read me" as two spoken words is usually "read me the output": it names
/// the readme only after "the". A single "readme" token always does.
fn is_read_me_phrase(span: &[Tok]) -> bool {
    span.len() == 2 && span[0].key == "read" && span[1].key == "me"
}

/// `x.yaml` ↔ `x.yml`.
fn yaml_twin(name: &str) -> Option<String> {
    if let Some(stem) = name.strip_suffix(".yaml") {
        return Some(format!("{stem}.yml"));
    }
    name.strip_suffix(".yml").map(|stem| format!("{stem}.yaml"))
}

/// Files named `name` whose directories end with the spoken ones. "source"
/// also matches `src`.
fn named_in(name: &str, dirs: &[String], index: &Index) -> Vec<u32> {
    let Some(parent) = dirs.last() else {
        return index.by_name(name).to_vec();
    };
    let mut candidates = index.by_parent(parent, name).to_vec();
    if parent == "source" {
        candidates.extend_from_slice(index.by_parent("src", name));
    }
    let found = filter_dirs(&candidates, dirs, index);
    if !found.is_empty() {
        return found;
    }
    // "source slash popover dot rs" for src/ui/popover.rs: the spoken
    // directories in order, with gaps. Only among a few same-named files.
    let named = index.by_name(name);
    if named.len() > MAX_LOOSE {
        return Vec::new();
    }
    named
        .iter()
        .copied()
        .filter(|&i| {
            let parts: Vec<&str> = index.path(i).split('/').collect();
            let mut have = parts[..parts.len() - 1].iter();
            dirs.iter().all(|said| have.any(|h| dir_matches(h, said)))
        })
        .collect()
}

/// Same-named files a loose (gapped) directory match looks through.
const MAX_LOOSE: usize = 64;

fn dir_matches(have: &str, said: &str) -> bool {
    squashes_to(have, said) || (said == "source" && have == "src")
}

fn filter_dirs(candidates: &[u32], dirs: &[String], index: &Index) -> Vec<u32> {
    candidates
        .iter()
        .copied()
        .filter(|&i| {
            let mut have = index.path(i).rsplit('/').skip(1);
            dirs.iter()
                .rev()
                .all(|said| have.next().is_some_and(|have| dir_matches(have, said)))
        })
        .collect()
}

/// `squash(have) == said`, without allocating.
fn squashes_to(have: &str, said: &str) -> bool {
    have.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .eq(said.chars())
}

/// One candidate, or the single shallowest one.
fn choose<'a>(candidates: &[u32], index: &'a Index) -> Option<&'a str> {
    match candidates {
        [] => None,
        [one] => Some(index.path(*one)),
        _ => {
            let depth = |i: &u32| index.path(*i).matches('/').count();
            let min = candidates.iter().map(depth).min()?;
            let mut shallowest = candidates.iter().filter(|i| depth(i) == min);
            let first = shallowest.next()?;
            shallowest.next().is_none().then(|| index.path(*first))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::apply_vocab;
    use super::super::testutil::vocab;

    const FILES: &[&str] = &[
        "Cargo.toml",
        "Cargo.lock",
        "CLAUDE.md",
        "README.md",
        "docs/README.md",
        "Makefile",
        "LICENSE",
        ".gitignore",
        "config.yml",
        "package.json",
        "src/main.rs",
        "src/audio.rs",
        "src/status_item.rs",
        "src/context/mod.rs",
        "src/store/mod.rs",
        "src/lib.rs",
        "crates/squawk-core/Cargo.toml",
        "crates/squawk-core/src/lib.rs",
        "crates/squawk-core/src/text.rs",
        "scripts/bundle",
        "web/ContentView.tsx",
        "db/migration_2.sql",
    ];

    fn run(input: &str) -> String {
        apply_vocab(input, &vocab(FILES, &[]))
    }

    #[test]
    fn resolves_spoken_file_names() {
        let cases = [
            ("look at audio dot rs", "look at @src/audio.rs"),
            ("look at audio.rs", "look at @src/audio.rs"),
            ("look at Audio.rs, then", "look at @src/audio.rs, then"),
            ("look at the cargo toml", "look at @Cargo.toml"),
            ("look at the cargo tomel", "look at @Cargo.toml"),
            ("open cargo dot toml", "open @Cargo.toml"),
            ("open Cargo.toml.", "open @Cargo.toml"),
            ("read the claude md first", "read @CLAUDE.md first"),
            ("read claude dot md", "read @CLAUDE.md"),
            ("update the readme", "update @README.md"),
            ("update README", "update @README.md"),
            ("update the read me", "update @README.md"),
            ("can you read me the readme", "can you read me @README.md"),
            ("fix status item dot rs", "fix @src/status_item.rs"),
            (
                "fix status underscore item dot rs",
                "fix @src/status_item.rs",
            ),
            ("in source slash main dot rs", "in @src/main.rs"),
            ("in src/main.rs", "in @src/main.rs"),
            ("see content view dot tsx", "see @web/ContentView.tsx"),
            ("edit the dot gitignore", "edit @.gitignore"),
            ("edit the gitignore", "edit @.gitignore"),
            ("and the cargo lock", "and @Cargo.lock"),
            ("the makefile is wrong", "@Makefile is wrong"),
            ("check config dot yaml", "check @config.yml"),
            ("check package dot jason", "check @package.json"),
            ("audio dot r s", "@src/audio.rs"),
            ("migration two dot sql", "@db/migration_2.sql"),
        ];
        for (input, want) in cases {
            assert_eq!(run(input), want, "{input}");
        }
    }

    #[test]
    fn disambiguates_by_path_or_depth() {
        let cases = [
            // Two mod.rs at the same depth: left alone.
            ("open mod dot rs", "open mod dot rs"),
            ("open context slash mod dot rs", "open @src/context/mod.rs"),
            ("open store/mod.rs", "open @src/store/mod.rs"),
            // Directories with a gap: src/…/mod.rs is still two files.
            (
                "open source slash mod dot rs",
                "open source slash mod dot rs",
            ),
            (
                "in crates slash text dot rs",
                "in @crates/squawk-core/src/text.rs",
            ),
            // lib.rs at src/ and deeper: the shallowest wins.
            ("open lib dot rs", "open @src/lib.rs"),
            (
                "open squawk core slash source slash lib dot rs",
                "open @crates/squawk-core/src/lib.rs",
            ),
            ("text dot rs", "@crates/squawk-core/src/text.rs"),
            // A spoken path with no ext names an extensionless file.
            ("run scripts slash bundle", "run @scripts/bundle"),
        ];
        for (input, want) in cases {
            assert_eq!(run(input), want, "{input}");
        }
    }

    #[test]
    fn at_sign_and_articles() {
        let cases = [
            ("at sign audio.rs", "@src/audio.rs"),
            ("at audio dot rs please", "@src/audio.rs please"),
            ("@ audio dot rs", "@src/audio.rs"),
            (
                "read audio.rs and at main dot rs",
                "read @src/audio.rs and @src/main.rs",
            ),
            ("look at audio dot rs", "look at @src/audio.rs"),
            ("the audio dot rs file is long", "@src/audio.rs is long"),
            ("open audio dot rs file", "open @src/audio.rs file"),
            ("(the audio dot rs)", "(@src/audio.rs)"),
        ];
        for (input, want) in cases {
            assert_eq!(run(input), want, "{input}");
        }
    }

    #[test]
    fn leaves_uncertain_things_alone() {
        let cases = [
            // Stems alone are English.
            "the audio is choppy",
            "we need a license",
            "use the main branch",
            "read me the output",
            "cargo is slow",
            "claude should know",
            // Not in the repo.
            "open widget dot rs",
            "src slash foo dot rs",
            "check audio dot rs dot orig",
            // Protected chunks it did not create.
            "@src/audio.rs is fine",
            "see https://example.com/audio.rs",
            "mail you@example.com",
            "open ~/notes/audio.rs",
            "run `cargo test`",
            // Extension words that are English.
            "let main go",
            "use a lock file",
        ];
        for input in cases {
            assert_eq!(run(input), input, "{input}");
        }
    }

    #[test]
    fn a_sentence_period_does_not_stick_to_the_mention() {
        assert_eq!(run("fix audio dot rs."), "fix @src/audio.rs");
        assert_eq!(
            run("fix audio dot rs. Then test"),
            "fix @src/audio.rs Then test"
        );
        assert_eq!(run("is it audio dot rs?"), "is it @src/audio.rs?");
    }

    #[test]
    fn several_in_one_sentence() {
        assert_eq!(
            run("compare audio dot rs with main dot rs and the cargo toml"),
            "compare @src/audio.rs with @src/main.rs and @Cargo.toml"
        );
    }

    #[test]
    fn no_files_no_work() {
        assert_eq!(
            apply_vocab("look at audio dot rs", &vocab(&[], &[])),
            "look at audio dot rs"
        );
    }
}
