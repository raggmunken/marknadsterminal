//! Chain engine step 4: the quote check.
//!
//! A relation is only kept if its quote occurs in the source text. The
//! comparison ignores differences in whitespace and in quote style, since
//! filings mix straight and typographic quotes. Nothing else is forgiven.

/// Shorter quotes are too easy to find by accident.
pub const MIN_QUOTE_CHARS: usize = 20;

#[derive(Debug, thiserror::Error, PartialEq, Eq, Clone, Copy)]
pub enum QuoteError {
    #[error("quote is shorter than {} characters", MIN_QUOTE_CHARS)]
    TooShort,
    #[error("quote does not occur in the source text")]
    NotFound,
}

/// Collapse whitespace and unify quote characters.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for c in text.chars() {
        let c = match c {
            '\u{2018}' | '\u{2019}' | '\u{201b}' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201f}' => '"',
            other => other,
        };
        if c.is_whitespace() {
            pending_space = !out.is_empty();
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(c);
        }
    }
    out
}

/// Check that `quote` occurs in `source`.
pub fn verify_quote(source: &str, quote: &str) -> Result<(), QuoteError> {
    let quote = normalize(quote);
    if quote.chars().count() < MIN_QUOTE_CHARS {
        return Err(QuoteError::TooShort);
    }
    if normalize(source).contains(&quote) {
        Ok(())
    } else {
        Err(QuoteError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "In fiscal 2025, Example Motors Corp. accounted for 21% of the Company\u{2019}s net sales.";

    #[test]
    fn accepts_verbatim_quote() {
        assert_eq!(verify_quote(SOURCE, "Example Motors Corp. accounted for 21%"), Ok(()));
    }

    #[test]
    fn forgives_whitespace_and_quote_style_only() {
        assert_eq!(
            verify_quote(SOURCE, "accounted for 21%  of the\nCompany's net sales"),
            Ok(())
        );
    }

    #[test]
    fn rejects_changed_number() {
        assert_eq!(
            verify_quote(SOURCE, "Example Motors Corp. accounted for 31%"),
            Err(QuoteError::NotFound)
        );
    }

    #[test]
    fn rejects_paraphrase() {
        assert_eq!(
            verify_quote(SOURCE, "Example Motors Corp. made up 21% of net sales"),
            Err(QuoteError::NotFound)
        );
    }

    #[test]
    fn rejects_short_quote_even_if_present() {
        assert_eq!(verify_quote(SOURCE, "accounted for 21%"), Err(QuoteError::TooShort));
    }

    #[test]
    fn rejects_empty_quote() {
        assert_eq!(verify_quote(SOURCE, "   "), Err(QuoteError::TooShort));
    }

    #[test]
    fn is_case_sensitive() {
        assert_eq!(
            verify_quote(SOURCE, "example motors corp. accounted for 21%"),
            Err(QuoteError::NotFound)
        );
    }
}
