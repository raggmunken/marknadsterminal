//! Company name normalisation, shared by matching, merging and scoring.

const LEGAL_SUFFIXES: &[&str] = &[
    "inc", "incorporated", "corp", "corporation", "co", "company", "ltd", "limited", "llc", "plc",
];

/// Lowercase, no punctuation, no trailing legal suffix.
///
/// `Example Motors Corp.` and `EXAMPLE MOTORS CORPORATION` both become
/// `example motors`. A name that is nothing but a suffix is kept as it is.
pub fn normalize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '&' { c.to_ascii_lowercase() } else { ' ' })
        .collect();
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    while words.len() > 1 && LEGAL_SUFFIXES.contains(words.last().expect("len checked")) {
        words.pop();
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_match_across_suffix_and_punctuation() {
        assert_eq!(normalize_name("Example Motors Corp."), "example motors");
        assert_eq!(normalize_name("EXAMPLE MOTORS CORPORATION"), "example motors");
        assert_eq!(normalize_name("AT&T Inc."), "at&t");
        assert_eq!(normalize_name("Example Holdings Co., Ltd."), "example holdings");
        assert_eq!(normalize_name("Co"), "co");
        assert_eq!(normalize_name("  "), "");
    }
}
