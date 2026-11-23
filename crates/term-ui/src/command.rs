//! The command line at the top of the terminal: `GM KEDJA`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum View {
    /// The chain: suppliers, the company, customers.
    Kedja,
    /// The evidence behind one relation.
    Bevis,
    /// The review queue.
    Granska,
    /// Events that may disturb a chain: earthquakes, cyclones, floods.
    Handelser,
}

impl View {
    pub const ALL: [View; 4] = [View::Kedja, View::Bevis, View::Granska, View::Handelser];

    pub fn title(self) -> &'static str {
        match self {
            View::Kedja => "KEDJA",
            View::Bevis => "BEVIS",
            View::Granska => "GRANSKA",
            View::Handelser => "HÄNDELSER",
        }
    }

    fn parse(word: &str) -> Option<View> {
        // The view can be typed with or without the Swedish letter.
        let upper = word.to_uppercase().replace('Ä', "A");
        View::ALL.into_iter().find(|v| v.title().replace('Ä', "A") == upper)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Open a company, given as ticker or CIK, in a view.
    Open { target: String, view: View },
    /// Bring a view to the front without changing company.
    Focus(View),
}

/// `GM KEDJA`, `GM` (the chain is the default view), `1467858 BEVIS`, `GRANSKA`.
pub fn parse(input: &str) -> Result<Command, String> {
    let words: Vec<&str> = input.split_whitespace().collect();
    match words.as_slice() {
        [] => Err("Skriv ett bolag och en vy, till exempel: GM KEDJA".to_string()),
        [one] => match View::parse(one) {
            Some(view) => Ok(Command::Focus(view)),
            None => Ok(Command::Open { target: one.to_uppercase(), view: View::Kedja }),
        },
        [target, view] => match View::parse(view) {
            Some(view) => Ok(Command::Open { target: target.to_uppercase(), view }),
            None => Err(format!(
                "Okänd vy {:?}. Vyerna är {}.",
                view,
                View::ALL.map(View::title).join(", ")
            )),
        },
        _ => Err("För många ord. Skriv bolag och vy, till exempel: GM KEDJA".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn company_and_view() {
        assert_eq!(parse("GM KEDJA"), Ok(Command::Open { target: "GM".into(), view: View::Kedja }));
        assert_eq!(parse("  gm   bevis "), Ok(Command::Open { target: "GM".into(), view: View::Bevis }));
        assert_eq!(parse("1467858 kedja"), Ok(Command::Open { target: "1467858".into(), view: View::Kedja }));
    }

    #[test]
    fn company_alone_opens_the_chain() {
        assert_eq!(parse("ntc"), Ok(Command::Open { target: "NTC".into(), view: View::Kedja }));
    }

    #[test]
    fn view_alone_only_changes_focus() {
        assert_eq!(parse("granska"), Ok(Command::Focus(View::Granska)));
    }

    #[test]
    fn swedish_letter_is_optional() {
        assert_eq!(parse("handelser"), Ok(Command::Focus(View::Handelser)));
        assert_eq!(parse("händelser"), Ok(Command::Focus(View::Handelser)));
        assert_eq!(parse("HÄNDELSER"), Ok(Command::Focus(View::Handelser)));
    }

    #[test]
    fn errors_say_what_to_type() {
        assert!(parse("").unwrap_err().contains("GM KEDJA"));
        assert!(parse("GM KURS").unwrap_err().contains("KEDJA, BEVIS, GRANSKA, HÄNDELSER"));
        assert!(parse("a b c").is_err());
    }
}
