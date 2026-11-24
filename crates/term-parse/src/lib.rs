//! Chain engine step 2: turn a filing into text and pick the passages
//! that are worth sending to the extractor.

use serde::Serialize;

/// Strip HTML down to text with one paragraph per line.
///
/// This is a small hand-written stripper, not an HTML parser. It handles
/// what annual reports need: tags, comments, script and style blocks and
/// character entities. Block level tags become line breaks.
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    while let Some(lt) = rest.find('<') {
        push_decoded(&mut out, &rest[..lt]);
        let after = &rest[lt + 1..];

        if let Some(comment) = after.strip_prefix("!--") {
            rest = match comment.find("-->") {
                Some(end) => &comment[end + 3..],
                None => "",
            };
            continue;
        }

        let Some(gt) = after.find('>') else {
            // Unterminated tag: drop the remainder instead of guessing.
            rest = "";
            break;
        };
        let tag = &after[..gt];
        let closing = tag.starts_with('/');
        let name = tag_name(tag);
        rest = &after[gt + 1..];

        if !closing && (name == "script" || name == "style") {
            rest = skip_past_closing(rest, &name);
            continue;
        }
        if is_block(&name) {
            out.push('\n');
        } else if name == "td" || name == "th" {
            out.push(' ');
        }
    }
    push_decoded(&mut out, rest);
    normalize_paragraphs(&out)
}

fn tag_name(tag: &str) -> String {
    tag.trim_start_matches('/')
        .split(|c: char| c.is_whitespace() || c == '/')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "br" | "tr" | "li" | "ul" | "ol" | "table" | "hr" | "title" | "section"
            | "article" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6"
    )
}

fn skip_past_closing<'a>(rest: &'a str, name: &str) -> &'a str {
    let needle = format!("</{name}");
    // ASCII lowercasing keeps byte offsets intact.
    match rest.to_ascii_lowercase().find(&needle) {
        Some(start) => match rest[start..].find('>') {
            Some(gt) => &rest[start + gt + 1..],
            None => "",
        },
        None => "",
    }
}

fn push_decoded(out: &mut String, text: &str) {
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        push_flat(out, &rest[..amp]);
        let tail = &rest[amp + 1..];
        let decoded = tail
            .find(';')
            .filter(|&end| end <= 10)
            .and_then(|end| decode_entity(&tail[..end]).map(|c| (c, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = tail;
            }
        }
    }
    push_flat(out, rest);
}

/// Line breaks inside the HTML source are layout, not paragraph breaks.
fn push_flat(out: &mut String, text: &str) {
    out.extend(text.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }));
}

fn decode_entity(entity: &str) -> Option<char> {
    if let Some(number) = entity.strip_prefix('#') {
        let code = match number.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => number.parse().ok()?,
        };
        return char::from_u32(code);
    }
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "ndash" => '\u{2013}',
        "mdash" => '\u{2014}',
        "lsquo" => '\u{2018}',
        "rsquo" => '\u{2019}',
        "ldquo" => '\u{201c}',
        "rdquo" => '\u{201d}',
        _ => return None,
    })
}

/// One paragraph per line, single spaces, no empty lines.
fn normalize_paragraphs(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let collapsed = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if !collapsed.is_empty() {
            lines.push(collapsed);
        }
    }
    lines.join("\n")
}

/// A paragraph that looks like it talks about customers or suppliers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Passage {
    /// Index of the paragraph in the text, counted from zero.
    pub index: usize,
    pub text: String,
    pub score: u32,
    /// The signals that fired, for debugging and for tuning the list.
    pub hits: Vec<&'static str>,
}

/// Default threshold for [`select_passages`].
pub const DEFAULT_MIN_SCORE: u32 = 4;

/// Paragraphs shorter than this are headings or table debris.
const MIN_PARAGRAPH_CHARS: usize = 40;

/// Signal phrases and their weights. Each phrase counts once per paragraph.
/// The list is a first guess and has not been tuned on real filings.
///
/// Selection leans towards letting too much through. A passage that should
/// not have been selected costs tokens. A passage that was skipped loses the
/// relation for good, since nothing downstream can bring it back. Phrases
/// that state a supplier relation on their own therefore reach the default
/// threshold alone.
const SIGNALS: &[(&str, u32)] = &[
    ("supplied by", 4),
    ("purchases from", 4),
    ("sourced from", 4),
    ("accounted for", 3),
    ("largest customer", 3),
    ("major customer", 3),
    ("significant customer", 3),
    ("sole source", 3),
    ("sole-source", 3),
    ("single source", 3),
    ("single-source", 3),
    ("sole supplier", 3),
    ("% of", 2),
    ("percent of", 2),
    ("concentration", 2),
    ("customer", 1),
    ("supplier", 1),
    ("net sales", 1),
    ("revenue", 1),
    ("rely on", 1),
    ("depend on", 1),
];

/// Signals for the language model path. Contracts, press releases, prospectuses and
/// proxy statements do not use the fixed phrases above, so the net is cast wider and a
/// model decides what the paragraph really says. Weights are rules of thumb.
const WIDE_SIGNALS: &[(&str, u32)] = &[
    ("supply agreement", 3),
    ("supply and", 2),
    ("purchase agreement", 3),
    ("master agreement", 2),
    ("offtake", 3),
    ("agreement with", 2),
    ("agreement between", 2),
    ("awarded", 2),
    ("selected as", 2),
    ("selected by", 2),
    ("contract with", 2),
    ("supplied by", 3),
    ("supplier", 2),
    ("suppliers", 1),
    ("supplies", 1),
    ("customer", 2),
    ("customers", 1),
    ("purchases from", 3),
    ("sourced from", 3),
    ("sole source", 3),
    ("single source", 3),
    ("manufactured by", 2),
    ("manufacturer", 1),
    ("smelter", 3),
    ("related party", 2),
    ("related person", 2),
    ("joint venture", 1),
    ("% of", 1),
    ("percent of", 1),
    ("accounted for", 2),
    ("largest", 1),
    ("long-term", 1),
    ("partner", 1),
];

/// Like [`select_passages`] with the wider signal set, keeping at most `max` paragraphs:
/// the highest scoring ones, returned in document order.
pub fn select_passages_wide(text: &str, min_score: u32, max: usize) -> Vec<Passage> {
    let mut all: Vec<Passage> = text
        .lines()
        .enumerate()
        .filter(|(_, paragraph)| paragraph.chars().count() >= MIN_PARAGRAPH_CHARS)
        .filter_map(|(index, paragraph)| {
            let lower = paragraph.to_lowercase();
            let mut score = 0;
            let mut hits = Vec::new();
            for &(phrase, weight) in WIDE_SIGNALS {
                if lower.contains(phrase) {
                    score += weight;
                    hits.push(phrase);
                }
            }
            (score >= min_score).then(|| Passage { index, text: paragraph.to_string(), score, hits })
        })
        .collect();
    if all.len() > max {
        all.sort_by(|a, b| b.score.cmp(&a.score).then(a.index.cmp(&b.index)));
        all.truncate(max);
        all.sort_by_key(|p| p.index);
    }
    all
}

/// Pick paragraphs whose signal score reaches `min_score`, in document order.
pub fn select_passages(text: &str, min_score: u32) -> Vec<Passage> {
    text.lines()
        .enumerate()
        .filter(|(_, paragraph)| paragraph.chars().count() >= MIN_PARAGRAPH_CHARS)
        .filter_map(|(index, paragraph)| {
            let lower = paragraph.to_lowercase();
            let mut score = 0;
            let mut hits = Vec::new();
            for &(phrase, weight) in SIGNALS {
                if lower.contains(phrase) {
                    score += weight;
                    hits.push(phrase);
                }
            }
            (score >= min_score).then(|| Passage {
                index,
                text: paragraph.to_string(),
                score,
                hits,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_selection_finds_contract_language_and_caps_the_count() {
        let contract = "The Company entered into a supply agreement with Polar Castings Ltd. under which Polar will supply housings for five years.";
        let filler = "This paragraph talks about the weather in the region and nothing else of any relevance to the business at all.";
        let text = format!("{filler}\n{contract}\n{filler}");
        assert!(select_passages(&text, DEFAULT_MIN_SCORE).is_empty());
        let wide = select_passages_wide(&text, 3, 10);
        assert_eq!(wide.len(), 1);
        assert_eq!(wide[0].text, contract);
        let many = vec![contract; 30].join("\n");
        assert_eq!(select_passages_wide(&many, 3, 5).len(), 5);
    }

    #[test]
    fn strips_tags_and_keeps_one_paragraph_per_line() {
        let html = "<html><body><h1>Item 1</h1><p>First   para\n continues.</p><div>Second</div></body></html>";
        assert_eq!(html_to_text(html), "Item 1\nFirst para continues.\nSecond");
    }

    #[test]
    fn drops_script_style_and_comments() {
        let html = "<p>Keep</p><SCRIPT>var x = '<p>no</p>';</SCRIPT><style>p{color:red}</style><!-- <p>hidden</p> --><p>Also keep</p>";
        assert_eq!(html_to_text(html), "Keep\nAlso keep");
    }

    #[test]
    fn decodes_entities() {
        let html = "<p>AT&amp;T&nbsp;Inc. &#8220;quoted&#8221; &#x26; R&D &unknown; 5 &lt; 6</p>";
        assert_eq!(
            html_to_text(html),
            "AT&T Inc. \u{201c}quoted\u{201d} & R&D &unknown; 5 < 6"
        );
    }

    #[test]
    fn table_cells_do_not_run_together() {
        let html = "<table><tr><td>Customer A</td><td>12%</td></tr></table>";
        assert_eq!(html_to_text(html), "Customer A 12%");
    }

    #[test]
    fn unterminated_tag_does_not_panic() {
        assert_eq!(html_to_text("<p>Fine</p><div class="), "Fine");
        assert_eq!(html_to_text("<script>never closed"), "");
    }

    #[test]
    fn selects_customer_paragraph_and_skips_boilerplate() {
        let text = "Forward looking statements involve risks and uncertainties that may differ.\n\
                    Example Motors Corp. accounted for 21% of our net sales in fiscal 2025.\n\
                    Customers";
        let passages = select_passages(text, DEFAULT_MIN_SCORE);
        assert_eq!(passages.len(), 1);
        assert_eq!(passages[0].index, 1);
        assert!(passages[0].hits.contains(&"accounted for"));
        assert!(passages[0].score >= 6);
    }

    #[test]
    fn selects_sole_source_supplier_paragraph() {
        let text = "Certain controllers are supplied by Example Semiconductor Ltd, which is our sole source for that part.";
        let passages = select_passages(text, DEFAULT_MIN_SCORE);
        assert_eq!(passages.len(), 1);
    }
}
