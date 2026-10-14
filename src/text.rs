//! Text post-processing: the pure-Rust passes and optional external cleanup
//! command applied to a finished transcript before it is output or recorded.
//!
//! The passes run in a fixed order - replacements, spoken punctuation, filler
//! removal, then the external command - so a mistranscription fixed by the
//! replacement table can still be punctuated, and the external command (a
//! local LLM cleanup pipe, say) sees the most-corrected text. Every pass is
//! local and dependency-free; the external command is the user's own, given
//! the transcript on stdin and read back from stdout, with a timeout that
//! falls back to the text without the command.
//!
//! Matching works on whole words, never inside them: the transcript is split
//! into alternating word and separator runs, so a replacement or a spoken
//! word can never turn part of `command` into `,`. Separators between the
//! words of a multi-word phrase (or key) must be plain whitespace.

use crate::config::TextConfig;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// One whole-word filler removed by `remove_fillers` when the dictation
/// language is English. Only unambiguous interjections are listed: a word that
/// is also meaningful in ordinary English (`like`, `right`, `actually`) would
/// delete real dictation, which is the failure mode this list must not have.
const ENGLISH_FILLERS: &[&str] = &[
    "um", "uh", "uhm", "umm", "uhh", "erm", "er", "hmm", "hmmm", "hm", "mmm",
];

/// Spoken words that become symbols when `spoken_punctuation` is on. The last
/// field is how the symbol joins the surrounding words: `Left` swallows the
/// space before it (a comma), `Right` swallows the space after it (an opening
/// bracket), `Both` swallows both (an operator or a newline), and `None`
/// leaves the surrounding whitespace untouched.
const SPOKEN_PUNCTUATION: &[(&[&str], &str, Attach)] = &[
    (&["comma"], ",", Attach::Left),
    (&["period"], ".", Attach::Left),
    (&["full", "stop"], ".", Attach::Left),
    (&["question", "mark"], "?", Attach::Left),
    (&["exclamation", "mark"], "!", Attach::Left),
    (&["exclamation", "point"], "!", Attach::Left),
    (&["colon"], ":", Attach::Left),
    (&["semicolon"], ";", Attach::Left),
    (&["ellipsis"], "...", Attach::Left),
    (&["new", "line"], "\n", Attach::Both),
    (&["newline"], "\n", Attach::Both),
    (&["new", "paragraph"], "\n\n", Attach::Both),
    (&["open", "paren"], "(", Attach::Right),
    (&["open", "parenthesis"], "(", Attach::Right),
    (&["close", "paren"], ")", Attach::Left),
    (&["close", "parenthesis"], ")", Attach::Left),
    (&["open", "bracket"], "[", Attach::Right),
    (&["close", "bracket"], "]", Attach::Left),
    (&["open", "brace"], "{", Attach::Right),
    (&["open", "curly", "brace"], "{", Attach::Right),
    (&["close", "brace"], "}", Attach::Left),
    (&["close", "curly", "brace"], "}", Attach::Left),
    (&["open", "quote"], "\"", Attach::Right),
    (&["close", "quote"], "\"", Attach::Left),
    (&["hyphen"], "-", Attach::Both),
    (&["dash"], "-", Attach::Both),
    (&["underscore"], "_", Attach::Both),
    (&["equals"], "=", Attach::Both),
    (&["equals", "sign"], "=", Attach::Both),
    (&["slash"], "/", Attach::Both),
    (&["backslash"], "\\", Attach::Both),
    (&["asterisk"], "*", Attach::Both),
    (&["star"], "*", Attach::Both),
    (&["percent"], "%", Attach::Both),
    (&["percent", "sign"], "%", Attach::Both),
    (&["plus"], "+", Attach::Both),
    (&["plus", "sign"], "+", Attach::Both),
    (&["at", "sign"], "@", Attach::Both),
    (&["hash"], "#", Attach::Both),
    (&["pound", "sign"], "#", Attach::Both),
    (&["number", "sign"], "#", Attach::Both),
    (&["ampersand"], "&", Attach::Both),
    (&["pipe"], "|", Attach::Both),
    (&["dollar", "sign"], "$", Attach::Both),
    (&["tilde"], "~", Attach::Both),
    (&["backtick"], "`", Attach::Both),
    (&["tab"], "\t", Attach::Both),
];

/// Applies every configured pass to `text`, in order: replacements, spoken
/// punctuation, English-only filler removal, then the external command.
/// `language` is `transcription.language`; filler removal is skipped unless it
/// names English, so it can never delete real words in another language.
pub fn post_process(text: &str, config: &TextConfig, language: &str) -> String {
    let mut result = text.to_string();
    if !config.replacements.is_empty() {
        result = apply_replacements(&result, &config.replacements);
    }
    if config.spoken_punctuation {
        result = apply_spoken_punctuation(&result);
    }
    if config.remove_fillers && is_english(language) {
        result = apply_filler_removal(&result);
    }
    let command = config.post_process_command.trim();
    if !command.is_empty() && !result.trim().is_empty() {
        let timeout = Duration::from_secs(config.post_process_timeout_seconds);
        match run_external_command(command, &result, timeout) {
            Some(cleaned) => result = cleaned,
            None => tracing::warn!(
                "text.post_process_command failed or timed out; using the text without it"
            ),
        }
    }
    result
}

/// Whether a `transcription.language` value names English: `en`, `english`,
/// or a regional form such as `en-US`. `auto` is deliberately not English,
/// because the language was not pinned and filler removal must stay safe.
fn is_english(language: &str) -> bool {
    let lower = language.trim().to_ascii_lowercase();
    let base = lower.split(['-', '_']).next().unwrap_or("");
    matches!(base, "en" | "english")
}

/// How a symbol joins the whitespace around the spoken words it replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Attach {
    None,
    Left,
    Right,
    Both,
}

/// A maximal run of letters/digits (`Word`) or of everything else (`Sep`),
/// so a phrase match can never land inside a word.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Sep(String),
}

impl Token {
    fn text(&self) -> &str {
        match self {
            Token::Word(text) | Token::Sep(text) => text,
        }
    }
}

/// Splits `text` into alternating word and separator runs. Runs never repeat a
/// kind, so the tokens alternate `Sep`/`Word` starting with whichever kind the
/// text begins with; that alternation is what lets a phrase be a plain token
/// slice.
fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut buffer = String::new();
    let mut buffer_is_word = false;
    for character in text.chars() {
        let is_word = character.is_alphanumeric();
        if buffer.is_empty() {
            buffer.push(character);
            buffer_is_word = is_word;
        } else if is_word == buffer_is_word {
            buffer.push(character);
        } else {
            tokens.push(push_token(&mut buffer, buffer_is_word));
            buffer.push(character);
            buffer_is_word = is_word;
        }
    }
    if !buffer.is_empty() {
        tokens.push(push_token(&mut buffer, buffer_is_word));
    }
    tokens
}

fn push_token(buffer: &mut String, is_word: bool) -> Token {
    let text = std::mem::take(buffer);
    if is_word {
        Token::Word(text)
    } else {
        Token::Sep(text)
    }
}

fn is_all_whitespace(text: &str) -> bool {
    text.chars().all(char::is_whitespace)
}

/// One replacement: `tokens[start..end]` becomes `replacement`, joined to the
/// surrounding text per `attach`. Edits are non-overlapping and applied in
/// order.
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
    attach: Attach,
}

/// Rebuilds the text from `tokens` with each edit applied. For `Left`/`Both`
/// the space or tab just before the edit is dropped (so `hello comma world`
/// becomes `hello, world`); for `Right`/`Both` the whitespace just after it is
/// dropped (so `open paren x` becomes `(x`). Newlines are never swallowed, so
/// a punctuation word at the start of a line keeps its position.
fn apply_edits(tokens: &[Token], edits: &[Edit]) -> String {
    let mut output = String::new();
    let mut index = 0;
    let mut edit_index = 0;
    while index < tokens.len() {
        if edit_index < edits.len() && edits[edit_index].start == index {
            let edit = &edits[edit_index];
            if matches!(edit.attach, Attach::Left | Attach::Both) {
                while output.ends_with(' ') || output.ends_with('\t') {
                    output.pop();
                }
            }
            output.push_str(&edit.replacement);
            index = edit.end;
            if matches!(edit.attach, Attach::Right | Attach::Both) {
                while index < tokens.len() {
                    match &tokens[index] {
                        Token::Sep(sep) if is_all_whitespace(sep) => index += 1,
                        _ => break,
                    }
                }
            }
            edit_index += 1;
        } else {
            output.push_str(tokens[index].text());
            index += 1;
        }
    }
    output
}

/// The replacement table applied case-insensitively, whole-word only, longest
/// key first. A key matches a run of tokens exactly, so `vox type` never
/// matches inside `vox typed` and a multi-word key keeps its own spacing.
fn apply_replacements(text: &str, replacements: &BTreeMap<String, String>) -> String {
    let tokens = tokenize(text);
    let mut patterns: Vec<(Vec<Token>, &str)> = replacements
        .iter()
        .filter(|(from, _)| !from.trim().is_empty())
        .map(|(from, to)| (tokenize(from.trim()), to.as_str()))
        .collect();
    patterns.sort_by(|left, right| {
        right
            .0
            .len()
            .cmp(&left.0.len())
            .then_with(|| pattern_weight(&right.0).cmp(&pattern_weight(&left.0)))
    });
    let mut edits = Vec::new();
    let mut index = 0;
    'outer: while index < tokens.len() {
        for (pattern, replacement) in &patterns {
            if let Some(end) = match_token_pattern(&tokens, index, pattern) {
                edits.push(Edit {
                    start: index,
                    end,
                    replacement: (*replacement).to_string(),
                    attach: Attach::None,
                });
                index = end;
                continue 'outer;
            }
        }
        index += 1;
    }
    apply_edits(&tokens, &edits)
}

/// Total characters in the pattern's word tokens, so a longer phrase wins over
/// a shorter one with the same token count.
fn pattern_weight(pattern: &[Token]) -> usize {
    pattern
        .iter()
        .filter(|token| matches!(token, Token::Word(_)))
        .map(|token| token.text().chars().count())
        .sum()
}

/// Matches `pattern` against `tokens` starting at `start`, returning the end
/// index (exclusive). Word tokens compare case-insensitively; a whitespace
/// separator token in the pattern matches any whitespace separator; any other
/// separator must match exactly.
fn match_token_pattern(tokens: &[Token], start: usize, pattern: &[Token]) -> Option<usize> {
    if pattern.is_empty() || start + pattern.len() > tokens.len() {
        return None;
    }
    for (offset, pattern_token) in pattern.iter().enumerate() {
        match (pattern_token, &tokens[start + offset]) {
            (Token::Word(pattern_word), Token::Word(text)) => {
                if !pattern_word.eq_ignore_ascii_case(text) {
                    return None;
                }
            }
            (Token::Sep(pattern_sep), Token::Sep(text)) => {
                if is_all_whitespace(pattern_sep) {
                    if !is_all_whitespace(text) {
                        return None;
                    }
                } else if pattern_sep != text {
                    return None;
                }
            }
            _ => return None,
        }
    }
    Some(start + pattern.len())
}

/// Matches a phrase of plain words (`comma`, `new line`) against consecutive
/// word tokens separated by whitespace, returning the end index (exclusive).
fn match_word_phrase(tokens: &[Token], start: usize, words: &[&str]) -> Option<usize> {
    let mut index = start;
    for (position, word) in words.iter().enumerate() {
        match tokens.get(index)? {
            Token::Word(text) if text.eq_ignore_ascii_case(word) => index += 1,
            _ => return None,
        }
        if position + 1 < words.len() {
            match tokens.get(index)? {
                Token::Sep(separator) if is_all_whitespace(separator) => index += 1,
                _ => return None,
            }
        }
    }
    Some(index)
}

fn apply_spoken_punctuation(text: &str) -> String {
    let tokens = tokenize(text);
    let mut entries: Vec<&(&[&str], &str, Attach)> = SPOKEN_PUNCTUATION.iter().collect();
    entries.sort_by(|left, right| {
        right
            .0
            .len()
            .cmp(&left.0.len())
            .then_with(|| words_weight(right.0).cmp(&words_weight(left.0)))
    });
    let mut edits = Vec::new();
    let mut index = 0;
    'outer: while index < tokens.len() {
        for (words, symbol, attach) in &entries {
            if let Some(end) = match_word_phrase(&tokens, index, words) {
                edits.push(Edit {
                    start: index,
                    end,
                    replacement: (*symbol).to_string(),
                    attach: *attach,
                });
                index = end;
                continue 'outer;
            }
        }
        index += 1;
    }
    apply_edits(&tokens, &edits)
}

fn words_weight(words: &[&str]) -> usize {
    words.iter().map(|word| word.chars().count()).sum()
}

/// Removes whole English filler words, taking the space or tab after each with
/// it so no double space is left behind; a trailing filler leaves no space. A
/// following separator that contains a newline is kept (only a space just
/// before it is dropped), so removing a filler can never merge two lines.
fn apply_filler_removal(text: &str) -> String {
    let tokens = tokenize(text);
    let mut output = String::new();
    let mut index = 0;
    while index < tokens.len() {
        if let Token::Word(word) = &tokens[index] {
            if ENGLISH_FILLERS
                .iter()
                .any(|filler| word.eq_ignore_ascii_case(filler))
            {
                index += 1;
                if let Some(Token::Sep(separator)) = tokens.get(index) {
                    if is_all_whitespace(separator) {
                        if separator.contains('\n') {
                            while output.ends_with(' ') || output.ends_with('\t') {
                                output.pop();
                            }
                        } else {
                            index += 1;
                        }
                    }
                }
                continue;
            }
        }
        output.push_str(tokens[index].text());
        index += 1;
    }
    while output.ends_with(' ') || output.ends_with('\t') {
        output.pop();
    }
    output
}

/// Runs `command` through `sh -c` with `text` on stdin, returning its stdout
/// (with one trailing newline dropped) on a clean exit. Any spawn error,
/// non-zero exit, read error, or deadline expiry returns `None`, so the caller
/// keeps the text it already had. The child runs in its own process group and
/// that group is killed whenever a pipe thread is still blocked at the
/// deadline, so a descendant the direct `sh` child left behind cannot keep the
/// wait unbounded on any exit path.
fn run_external_command(command: &str, text: &str, timeout: Duration) -> Option<String> {
    let mut builder = Command::new("sh");
    builder
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        builder.process_group(0);
    }
    let mut child = builder.spawn().ok()?;
    let mut stdin = child.stdin.take()?;
    let stdout = child.stdout.take()?;
    let input = text.to_string();
    let (writer_done, writer_finished) = std::sync::mpsc::channel();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(input.as_bytes());
        // Dropping `stdin` here closes the pipe so the command sees EOF.
        let _ = writer_done.send(());
    });
    let (output_tx, output_rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let _ = output_tx.send(read_to_end(stdout));
    });
    let deadline = Instant::now().checked_add(timeout)?;
    let status = wait_until(&mut child, deadline);
    // A descendant that inherited a pipe but outlived the direct `sh` child
    // would keep these joins blocked, so bound both by the same deadline and
    // kill the group once either is still waiting.
    let bytes = output_rx.recv_timeout(until(deadline));
    if bytes.is_err() || writer_finished.recv_timeout(until(deadline)).is_err() {
        kill_child_group(&mut child);
    }
    let _ = writer.join();
    let _ = reader.join();
    let bytes = bytes.ok()?;
    match status {
        Some(status) if status.success() => {
            let output = String::from_utf8(bytes).ok()?;
            Some(output.strip_suffix('\n').unwrap_or(&output).to_string())
        }
        _ => None,
    }
}

fn until(deadline: Instant) -> Duration {
    deadline.saturating_duration_since(Instant::now())
}

fn read_to_end(mut stdout: impl Read) -> Vec<u8> {
    let mut buffer = Vec::new();
    let _ = stdout.read_to_end(&mut buffer);
    buffer
}

/// Waits for `child` to exit, killing it once `deadline` passes. `None` means
/// the child was killed for exceeding the deadline (or `try_wait` failed).
fn wait_until(child: &mut Child, deadline: Instant) -> Option<ExitStatus> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    kill_child_group(child);
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                kill_child_group(child);
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Kills the child and its whole process group. The child is spawned in its
/// own group (`process_group(0)`), so a descendant that inherited stdout (a
/// pipeline stage, or a command that forked) dies too and cannot keep the
/// stdout pipe open after the direct `sh` child is killed.
fn kill_child_group(child: &mut Child) {
    #[cfg(unix)]
    {
        let pid = child.id() as i32;
        // SAFETY: `pid` is this child's own process-group id because it was
        // spawned with `process_group(0)`, and it has not been reaped yet, so a
        // negative pid addresses exactly that group.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> TextConfig {
        TextConfig::default()
    }

    #[test]
    fn replacements_match_whole_words_case_insensitively() {
        let mut config = config();
        config
            .replacements
            .insert("vox type".to_string(), "Voxtype".to_string());
        config
            .replacements
            .insert("tee vee".to_string(), "TV".to_string());
        assert_eq!(
            post_process("I use vox type and Tee Vee daily.", &config, "en"),
            "I use Voxtype and TV daily."
        );
        // `vox typed` must not be truncated into `Voxtyped`.
        assert_eq!(
            post_process("we vox typed it", &config, "en"),
            "we vox typed it"
        );
    }

    #[test]
    fn a_replacement_result_is_never_re_matched_by_another_key() {
        let mut config = config();
        config.replacements.insert("a".to_string(), "b".to_string());
        config.replacements.insert("b".to_string(), "c".to_string());
        assert_eq!(post_process("a b", &config, "en"), "b c");
    }

    #[test]
    fn spoken_punctuation_joins_words_as_symbols() {
        let mut config = config();
        config.spoken_punctuation = true;
        assert_eq!(
            post_process("hello comma world period", &config, "en"),
            "hello, world."
        );
        assert_eq!(
            post_process("call open paren x close paren", &config, "en"),
            "call (x)"
        );
        assert_eq!(
            post_process("line one new line line two", &config, "en"),
            "line one\nline two"
        );
        assert_eq!(post_process("a equals b", &config, "en"), "a=b");
        assert_eq!(
            post_process("list open bracket one close bracket", &config, "en"),
            "list [one]"
        );
        assert_eq!(
            post_process("open brace x close brace", &config, "en"),
            "{x}"
        );
        assert_eq!(
            post_process("say open quote hi close quote", &config, "en"),
            "say \"hi\""
        );
        // A spoken word inside a real word is never rewritten.
        assert_eq!(post_process("the command", &config, "en"), "the command");
    }

    #[test]
    fn spoken_punctuation_is_off_by_default() {
        let config = config();
        assert_eq!(
            post_process("hello comma world", &config, "en"),
            "hello comma world"
        );
    }

    #[test]
    fn filler_removal_only_runs_for_english() {
        let mut config = config();
        config.remove_fillers = true;
        assert_eq!(
            post_process("um hello uh world hmm", &config, "en"),
            "hello world"
        );
        assert_eq!(post_process("um hello", &config, "en-US"), "hello");
        // A non-English dictation keeps every word, so real words are safe.
        assert_eq!(
            post_process("um hello uh world", &config, "de"),
            "um hello uh world"
        );
        // `auto` is not pinned to English, so removal stays off.
        assert_eq!(post_process("um hello", &config, "auto"), "um hello");
    }

    #[test]
    fn filler_removal_leaves_no_double_or_trailing_space() {
        let mut config = config();
        config.remove_fillers = true;
        assert_eq!(post_process("um uh hello", &config, "en"), "hello");
        assert_eq!(post_process("hello um", &config, "en"), "hello");
        assert_eq!(post_process("well um yes", &config, "en"), "well yes");
    }

    #[test]
    fn filler_removal_never_eats_a_line_break() {
        let mut config = config();
        config.remove_fillers = true;
        assert_eq!(post_process("one um\nconst", &config, "en"), "one\nconst");
        config.spoken_punctuation = true;
        assert_eq!(
            post_process("one um new line const", &config, "en"),
            "one\nconst"
        );
    }

    #[test]
    fn a_failing_post_process_command_falls_back_to_the_cleaned_text() {
        let mut config = config();
        config
            .replacements
            .insert("vox type".to_string(), "Voxtype".to_string());
        config.post_process_command = "exit 3".to_string();
        config.post_process_timeout_seconds = 5;
        assert_eq!(
            post_process("use vox type", &config, "en"),
            "use Voxtype",
            "a non-zero exit keeps the text the deterministic passes produced"
        );
    }

    #[test]
    fn a_timing_out_post_process_command_falls_back_safely() {
        let mut config = config();
        config.post_process_command = "sleep 30".to_string();
        config.post_process_timeout_seconds = 1;
        let started = Instant::now();
        assert_eq!(post_process("keep me", &config, "en"), "keep me");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the timeout must actually bound the wait"
        );
    }

    #[test]
    fn a_timing_out_command_kills_descendants_that_hold_the_output_pipe() {
        let mut config = config();
        config.post_process_command = "sleep 30 | cat".to_string();
        config.post_process_timeout_seconds = 1;
        let started = Instant::now();
        assert_eq!(post_process("keep me", &config, "en"), "keep me");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a surviving pipeline stage must not keep the reader blocked"
        );
    }

    #[test]
    fn a_command_that_leaves_a_descendant_holding_stdout_stays_bounded() {
        let mut config = config();
        config.post_process_command = "sleep 30 &".to_string();
        config.post_process_timeout_seconds = 1;
        let started = Instant::now();
        assert_eq!(post_process("keep me", &config, "en"), "keep me");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "a backgrounded descendant holding stdout must not keep the reader blocked"
        );
    }

    #[test]
    fn the_external_command_is_skipped_when_the_passes_empty_the_text() {
        let mut filler_config = config();
        filler_config.remove_fillers = true;
        filler_config.post_process_command = "echo hallucinated".to_string();
        filler_config.post_process_timeout_seconds = 5;
        assert_eq!(post_process("um", &filler_config, "en"), "");

        let mut replacement_config = config();
        replacement_config
            .replacements
            .insert("hello world".to_string(), String::new());
        replacement_config.post_process_command = "echo hallucinated".to_string();
        replacement_config.post_process_timeout_seconds = 5;
        assert_eq!(post_process("hello world", &replacement_config, "en"), "");
    }

    #[test]
    fn a_succeeding_post_process_command_replaces_the_text() {
        let mut config = config();
        config.post_process_command = "tr a-z A-Z".to_string();
        config.post_process_timeout_seconds = 5;
        assert_eq!(post_process("hello there", &config, "en"), "HELLO THERE");
    }

    #[test]
    fn english_language_detection_accepts_codes_and_names() {
        for english in ["en", "EN", "english", "en-US", "en_US"] {
            assert!(is_english(english), "{english}");
        }
        for other in ["auto", "de", "fr", "eng", "enigma"] {
            assert!(!is_english(other), "{other}");
        }
    }
}
