//! Strict `hidutil` `UserKeyMapping` parsing, classification and edit plans.
//!
//! `hidutil property --get UserKeyMapping` prints an `OpenStep` property list.
//! This parser accepts only the shapes it understands completely and refuses
//! everything else, because a guess about a global keyboard mapping could
//! overwrite or lose someone else's remap. Accepted:
//!
//! * the exact text `(null)`, meaning no mapping;
//! * an array of dictionaries, `( { ... }, { ... } )`, optionally wrapped as
//!   `{ UserKeyMapping = ( ... ); }`;
//! * dictionaries with exactly one `HIDKeyboardModifierMappingSrc` and one
//!   `HIDKeyboardModifierMappingDst`, each an unquoted unsigned decimal.
//!
//! Unknown keys, quoted values, hexadecimal, nulls inside the structure,
//! truncated or trailing text, non-UTF-8 bytes, more than [`MAX_ENTRIES`]
//! entries and more than one Caps Lock source are errors.

use std::fmt;

/// HID usage (page 7, usage 0x39) of the physical Caps Lock key.
pub const CAPS_LOCK_USAGE: u64 = 0x7_0000_0039;
/// HID usage (page 7, usage 0x6D) of F18, the Caps Lock surrogate.
pub const F18_USAGE: u64 = 0x7_0000_006D;
/// Most entries one mapping may have.
pub const MAX_ENTRIES: usize = 1_024;

const SRC_KEY: &str = "HIDKeyboardModifierMappingSrc";
const DST_KEY: &str = "HIDKeyboardModifierMappingDst";
const ARRAY_KEY: &str = "UserKeyMapping";

/// One `UserKeyMapping` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mapping {
    /// Source HID usage.
    pub src: u64,
    /// Destination HID usage.
    pub dst: u64,
}

/// The one mapping this application owns: Caps Lock to F18.
pub const OWNED: Mapping = Mapping {
    src: CAPS_LOCK_USAGE,
    dst: F18_USAGE,
};

/// Why `hidutil` output was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MappingError {
    /// The bytes are not UTF-8 or contain a NUL.
    NotText,
    /// The text ended inside a structure or is empty.
    Truncated,
    /// Text remained after a complete value.
    Trailing,
    /// A token or shape is not part of the accepted subset.
    Malformed(String),
    /// A dictionary key other than the source and destination keys.
    UnknownKey(String),
    /// A dictionary repeats a key.
    DuplicateKey(String),
    /// A dictionary lacks the source or the destination.
    MissingField(&'static str),
    /// A value is quoted.
    Quoted,
    /// A null appears where a dictionary or number is required.
    Null,
    /// A value is not an unsigned decimal number that fits 64 bits.
    BadNumber(String),
    /// More than [`MAX_ENTRIES`] entries.
    TooMany,
    /// The Caps Lock source appears more than once.
    DuplicateCapsSource,
}

impl fmt::Display for MappingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotText => f.write_str("hidutil output is not plain UTF-8 text"),
            Self::Truncated => f.write_str("hidutil output is empty or truncated"),
            Self::Trailing => f.write_str("hidutil output has trailing text"),
            Self::Malformed(what) => write!(f, "hidutil output is malformed: {what}"),
            Self::UnknownKey(key) => write!(f, "hidutil output has the unknown key `{key}`"),
            Self::DuplicateKey(key) => write!(f, "hidutil entry repeats `{key}`"),
            Self::MissingField(key) => write!(f, "hidutil entry lacks `{key}`"),
            Self::Quoted => f.write_str("hidutil output has a quoted value"),
            Self::Null => f.write_str("hidutil output has a null inside the mapping"),
            Self::BadNumber(text) => write!(f, "hidutil output has the bad number `{text}`"),
            Self::TooMany => write!(f, "hidutil output has more than {MAX_ENTRIES} entries"),
            Self::DuplicateCapsSource => {
                f.write_str("hidutil output maps the Caps Lock source more than once")
            }
        }
    }
}

impl std::error::Error for MappingError {}

/// The state of the Caps Lock source in a parsed mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapsState {
    /// No entry has the Caps Lock source.
    Absent,
    /// The entry is exactly Caps Lock to F18.
    Surrogate,
    /// The entry maps Caps Lock somewhere else.
    Foreign(u64),
}

/// Classifies the Caps Lock source. A parsed mapping has at most one.
#[must_use]
pub fn caps_state(mappings: &[Mapping]) -> CapsState {
    match mappings.iter().find(|m| m.src == CAPS_LOCK_USAGE) {
        None => CapsState::Absent,
        Some(m) if m.dst == F18_USAGE => CapsState::Surrogate,
        Some(m) => CapsState::Foreign(m.dst),
    }
}

/// The mapping with the owned entry appended and every other entry unchanged.
#[must_use]
pub fn with_surrogate(mappings: &[Mapping]) -> Vec<Mapping> {
    let mut next: Vec<Mapping> =
        mappings.iter().copied().filter(|m| m.src != CAPS_LOCK_USAGE).collect();
    next.push(OWNED);
    next
}

/// The mapping with only the exact owned entry removed.
#[must_use]
pub fn without_surrogate(mappings: &[Mapping]) -> Vec<Mapping> {
    mappings.iter().copied().filter(|m| *m != OWNED).collect()
}

/// The JSON payload for `hidutil property --set`.
#[must_use]
pub fn render_set_payload(mappings: &[Mapping]) -> String {
    let entries: Vec<String> = mappings
        .iter()
        .map(|m| format!("{{\"{SRC_KEY}\":{},\"{DST_KEY}\":{}}}", m.src, m.dst))
        .collect();
    format!("{{\"{ARRAY_KEY}\":[{}]}}", entries.join(","))
}

/// Parses complete `hidutil property --get UserKeyMapping` output.
///
/// # Errors
/// Returns the first [`MappingError`]; nothing is guessed.
pub fn parse_user_key_mapping(bytes: &[u8]) -> Result<Vec<Mapping>, MappingError> {
    if bytes.contains(&0) {
        return Err(MappingError::NotText);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| MappingError::NotText)?;
    if text.trim() == "(null)" {
        return Ok(Vec::new());
    }
    let mut parser = Parser { tokens: tokenize(text)?, at: 0 };
    let mappings = parser.document()?;
    if mappings.iter().filter(|m| m.src == CAPS_LOCK_USAGE).count() > 1 {
        return Err(MappingError::DuplicateCapsSource);
    }
    Ok(mappings)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Open,
    Close,
    BraceOpen,
    BraceClose,
    Equals,
    Semicolon,
    Comma,
    Word(String),
}

fn tokenize(text: &str) -> Result<Vec<Token>, MappingError> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        match c {
            c if c.is_ascii_whitespace() => {}
            '(' => tokens.push(Token::Open),
            ')' => tokens.push(Token::Close),
            '{' => tokens.push(Token::BraceOpen),
            '}' => tokens.push(Token::BraceClose),
            '=' => tokens.push(Token::Equals),
            ';' => tokens.push(Token::Semicolon),
            ',' => tokens.push(Token::Comma),
            '"' | '\'' => return Err(MappingError::Quoted),
            c if c.is_ascii_alphanumeric() || c == '_' => {
                let mut word = String::from(c);
                while let Some(&(_, next)) = chars.peek() {
                    if next.is_ascii_alphanumeric() || next == '_' {
                        word.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                tokens.push(Token::Word(word));
            }
            other => return Err(MappingError::Malformed(format!("unexpected `{other}`"))),
        }
        if tokens.len() > MAX_ENTRIES * 16 {
            return Err(MappingError::TooMany);
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<Token>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> { self.tokens.get(self.at) }

    fn next(&mut self) -> Result<Token, MappingError> {
        let token = self.tokens.get(self.at).cloned().ok_or(MappingError::Truncated)?;
        self.at += 1;
        Ok(token)
    }

    fn expect(&mut self, wanted: &Token) -> Result<(), MappingError> {
        match self.next()? {
            token if &token == wanted => Ok(()),
            Token::Word(word) if word == "null" => Err(MappingError::Null),
            other => Err(MappingError::Malformed(format!(
                "expected {wanted:?}, found {other:?}"
            ))),
        }
    }

    fn document(&mut self) -> Result<Vec<Mapping>, MappingError> {
        let mappings = match self.peek() {
            None => return Err(MappingError::Truncated),
            Some(Token::Open) => self.array()?,
            Some(Token::BraceOpen) => {
                self.next()?;
                match self.next()? {
                    Token::Word(key) if key == ARRAY_KEY => {}
                    Token::Word(key) => return Err(MappingError::UnknownKey(key)),
                    other => {
                        return Err(MappingError::Malformed(format!("unexpected {other:?}")));
                    }
                }
                self.expect(&Token::Equals)?;
                let mappings = self.array()?;
                self.expect(&Token::Semicolon)?;
                self.expect(&Token::BraceClose)?;
                mappings
            }
            Some(other) => {
                return Err(MappingError::Malformed(format!("unexpected {other:?}")));
            }
        };
        if self.peek().is_some() {
            return Err(MappingError::Trailing);
        }
        Ok(mappings)
    }

    fn array(&mut self) -> Result<Vec<Mapping>, MappingError> {
        self.expect(&Token::Open)?;
        let mut entries = Vec::new();
        loop {
            if self.peek() == Some(&Token::Close) {
                self.next()?;
                return Ok(entries);
            }
            if entries.len() >= MAX_ENTRIES {
                return Err(MappingError::TooMany);
            }
            entries.push(self.entry()?);
            match self.next()? {
                Token::Comma => {}
                Token::Close => return Ok(entries),
                other => {
                    return Err(MappingError::Malformed(format!("unexpected {other:?}")));
                }
            }
        }
    }

    fn entry(&mut self) -> Result<Mapping, MappingError> {
        self.expect(&Token::BraceOpen)?;
        let (mut src, mut dst) = (None, None);
        loop {
            let key = match self.next()? {
                Token::BraceClose => break,
                Token::Word(key) if key == "null" => return Err(MappingError::Null),
                Token::Word(key) => key,
                other => {
                    return Err(MappingError::Malformed(format!("unexpected {other:?}")));
                }
            };
            let slot = match key.as_str() {
                SRC_KEY => &mut src,
                DST_KEY => &mut dst,
                _ => return Err(MappingError::UnknownKey(key)),
            };
            self.expect(&Token::Equals)?;
            let value = match self.next()? {
                Token::Word(word) if word == "null" => return Err(MappingError::Null),
                Token::Word(word) => number(&word)?,
                other => {
                    return Err(MappingError::Malformed(format!("unexpected {other:?}")));
                }
            };
            if slot.replace(value).is_some() {
                return Err(MappingError::DuplicateKey(key));
            }
            self.expect(&Token::Semicolon)?;
        }
        Ok(Mapping {
            src: src.ok_or(MappingError::MissingField(SRC_KEY))?,
            dst: dst.ok_or(MappingError::MissingField(DST_KEY))?,
        })
    }
}

fn number(word: &str) -> Result<u64, MappingError> {
    if word.is_empty() || !word.bytes().all(|b| b.is_ascii_digit()) {
        return Err(MappingError::BadNumber(word.to_owned()));
    }
    word.parse().map_err(|_| MappingError::BadNumber(word.to_owned()))
}

/// Renders mappings the way `hidutil property --get` does. Test support.
#[cfg(any(test, feature = "testing"))]
#[must_use]
pub fn render_openstep(mappings: &[Mapping]) -> String {
    if mappings.is_empty() {
        return "(null)\n".to_owned();
    }
    let entries: Vec<String> = mappings
        .iter()
        .map(|m| {
            format!(
                "        {{\n        {DST_KEY} = {};\n        {SRC_KEY} = {};\n    }}",
                m.dst, m.src
            )
        })
        .collect();
    format!("(\n{}\n)\n", entries.join(",\n"))
}
