//! Workspace naming: "one", "two", "twenty-one".
//!
//! Numbers use word forms following the books' convention ("One Esk Nineteen"), which is why
//! workspace slots are named rather than numbered.

use std::collections::HashMap;
use std::sync::OnceLock;

/// Max number that gets a word name (1-99 use English words, 100+ use digits)
const MAX_WORD_NUMBER: u32 = 99;

/// Convert a number to its word form (1 -> "One", 21 -> "Twenty-One", etc.)
pub fn number_to_word(n: u32) -> String {
    if n == 0 {
        return "Zero".to_string();
    }
    if n <= MAX_WORD_NUMBER {
        english_numbers::convert(n as i64, english_numbers::Formatting::all())
    } else {
        n.to_string()
    }
}

/// Lazily-built reverse map from lowercase word form to number
fn word_to_number_map() -> &'static HashMap<String, u32> {
    static MAP: OnceLock<HashMap<String, u32>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();
        m.insert("zero".to_string(), 0);
        for n in 1..=MAX_WORD_NUMBER {
            let word = english_numbers::convert(n as i64, english_numbers::Formatting::all());
            m.insert(word.to_lowercase(), n);
        }
        m
    })
}

/// Convert a word to its number form ("One" -> 1, "Twenty-One" -> 21, etc.)
pub fn word_to_number(word: &str) -> Option<u32> {
    if let Some(&n) = word_to_number_map().get(&word.to_lowercase()) {
        return Some(n);
    }
    // Plain numbers ("100") name workspaces beyond the word range.
    word.parse::<u32>().ok()
}

/// Display name for a workspace, e.g. ("toren", 1) -> "Toren One".
pub fn ancillary_id(segment: &str, number: u32) -> String {
    format!("{} {}", capitalize(segment), number_to_word(number))
}

/// Extract the number from a display name.
pub fn ancillary_number(ancillary_id: &str) -> Option<u32> {
    ancillary_id
        .split_whitespace()
        .last()
        .and_then(word_to_number)
}

/// Extract the segment from a display name (lowercased).
pub fn ancillary_segment(ancillary_id: &str) -> Option<String> {
    ancillary_id
        .split_whitespace()
        .next()
        .map(|s| s.to_lowercase())
}

pub fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().chain(chars).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_become_words() {
        assert_eq!(number_to_word(1), "One");
        assert_eq!(number_to_word(21), "Twenty-One");
        assert_eq!(number_to_word(99), "Ninety-Nine");
        assert_eq!(number_to_word(100), "100");
    }

    #[test]
    fn words_become_numbers() {
        assert_eq!(word_to_number("One"), Some(1));
        assert_eq!(word_to_number("twenty-one"), Some(21));
        assert_eq!(word_to_number("100"), Some(100));
        assert_eq!(word_to_number("invalid"), None);
    }

    #[test]
    fn ancillary_ids_round_trip() {
        assert_eq!(ancillary_id("toren", 21), "Toren Twenty-One");
        assert_eq!(ancillary_number("Toren Twenty-One"), Some(21));
        assert_eq!(ancillary_segment("Toren One").as_deref(), Some("toren"));
    }
}
