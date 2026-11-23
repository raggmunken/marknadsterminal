//! The four gates a rule passes before its signals are shown as approved.
//!
//! 1. Hypothesis: written down before the first test.
//! 2. Historical test: point in time data, delisted stocks included, costs
//!    deducted, t value above 3.0.
//! 3. Paper test: signals logged forward with no money for at least 28 weeks.
//! 4. Approval: a named person decides.
//!
//! The t value threshold comes from Harvey, Liu and Zhu (2016). The 28
//! weeks and the minimum number of observations are the plan's own rules
//! of thumb, not research findings.

use serde::{Deserialize, Serialize};

pub const MIN_T_VALUE: f64 = 3.0;
pub const MIN_BACKTEST_OBSERVATIONS: u32 = 30;
pub const MIN_PAPER_WEEKS: u32 = 28;

/// Strictly greater. A value that is not a number never exceeds anything,
/// so a broken calculation cannot pass a gate.
fn exceeds(value: f64, floor: f64) -> bool {
    value.partial_cmp(&floor) == Some(std::cmp::Ordering::Greater)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stage {
    Hypothesis,
    Backtested,
    Paper,
    Approved,
    Retired,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BacktestResult {
    pub observations: u32,
    pub t_value: f64,
    /// Mean return per signal after costs.
    pub mean_net_return: f64,
    /// The test only read rows that were known at each test date.
    pub point_in_time: bool,
    /// The price history contains stocks that were later delisted.
    pub includes_delisted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PaperResult {
    pub weeks: u32,
    pub signals: u32,
    pub mean_net_return: f64,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum GateError {
    #[error("a hypothesis needs an id and a text")]
    EmptyHypothesis,
    #[error("rule is at stage {at:?}, this step needs {needs:?}")]
    WrongStage { at: Stage, needs: Stage },
    #[error("gate not passed: {}", .0.join("; "))]
    NotPassed(Vec<String>),
    #[error("approval needs the name of the person who approves")]
    NoApprover,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub id: String,
    pub version: u32,
    pub hypothesis: String,
    pub created_at: i64,
    /// Every historical test counts, including the ones that failed. The
    /// more variants tried, the less a single pass means.
    pub variants_tested: u32,
    pub stage: Stage,
    pub backtest: Option<BacktestResult>,
    pub paper: Option<PaperResult>,
    pub approved_by: Option<String>,
    pub retired_because: Option<String>,
}

impl Rule {
    /// Gate 1.
    pub fn new(id: &str, hypothesis: &str, created_at: i64) -> Result<Self, GateError> {
        if id.trim().is_empty() || hypothesis.trim().is_empty() {
            return Err(GateError::EmptyHypothesis);
        }
        Ok(Self {
            id: id.trim().to_string(),
            version: 1,
            hypothesis: hypothesis.trim().to_string(),
            created_at,
            variants_tested: 0,
            stage: Stage::Hypothesis,
            backtest: None,
            paper: None,
            approved_by: None,
            retired_because: None,
        })
    }

    fn require(&self, needs: Stage) -> Result<(), GateError> {
        if self.stage == needs {
            Ok(())
        } else {
            Err(GateError::WrongStage { at: self.stage, needs })
        }
    }

    /// Gate 2. The attempt is counted whether it passes or not.
    pub fn record_backtest(&mut self, result: BacktestResult) -> Result<(), GateError> {
        self.require(Stage::Hypothesis)?;
        self.variants_tested += 1;

        let mut failed = Vec::new();
        if !result.point_in_time {
            failed.push("the test was not point in time".to_string());
        }
        if !result.includes_delisted {
            failed.push("the price history has no delisted stocks".to_string());
        }
        if result.observations < MIN_BACKTEST_OBSERVATIONS {
            failed.push(format!("{} observations, needs {MIN_BACKTEST_OBSERVATIONS}", result.observations));
        }
        if !exceeds(result.t_value, MIN_T_VALUE) {
            failed.push(format!("t value {:.2}, needs above {MIN_T_VALUE}", result.t_value));
        }
        if !exceeds(result.mean_net_return, 0.0) {
            failed.push("nothing left after costs".to_string());
        }
        self.backtest = Some(result);
        if !failed.is_empty() {
            return Err(GateError::NotPassed(failed));
        }
        self.stage = Stage::Backtested;
        Ok(())
    }

    /// Start of gate 3.
    pub fn start_paper(&mut self) -> Result<(), GateError> {
        self.require(Stage::Backtested)?;
        self.stage = Stage::Paper;
        Ok(())
    }

    /// Update the running paper result. Does not change the stage.
    pub fn record_paper(&mut self, result: PaperResult) -> Result<(), GateError> {
        self.require(Stage::Paper)?;
        self.paper = Some(result);
        Ok(())
    }

    /// Gate 4. Only a person can call this, and the name is kept.
    pub fn approve(&mut self, approver: &str) -> Result<(), GateError> {
        self.require(Stage::Paper)?;
        if approver.trim().is_empty() {
            return Err(GateError::NoApprover);
        }
        let mut failed = Vec::new();
        match &self.paper {
            None => failed.push("no paper result recorded".to_string()),
            Some(p) => {
                if p.weeks < MIN_PAPER_WEEKS {
                    failed.push(format!("{} weeks on paper, needs {MIN_PAPER_WEEKS}", p.weeks));
                }
                if p.signals == 0 {
                    failed.push("no paper signals".to_string());
                }
                if !exceeds(p.mean_net_return, 0.0) {
                    failed.push("paper result is not positive after costs".to_string());
                }
            }
        }
        if !failed.is_empty() {
            return Err(GateError::NotPassed(failed));
        }
        self.approved_by = Some(approver.trim().to_string());
        self.stage = Stage::Approved;
        Ok(())
    }

    /// Allowed from any stage. A retired rule stays retired.
    pub fn retire(&mut self, reason: &str) {
        self.stage = Stage::Retired;
        self.retired_because = Some(reason.trim().to_string());
    }

    /// A changed rule is a new rule: it starts over at gate 1 with a new
    /// version number, and keeps the count of variants tried.
    pub fn revise(&self, hypothesis: &str, created_at: i64) -> Result<Self, GateError> {
        let mut next = Rule::new(&self.id, hypothesis, created_at)?;
        next.version = self.version + 1;
        next.variants_tested = self.variants_tested;
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn good_backtest() -> BacktestResult {
        BacktestResult {
            observations: 120,
            t_value: 3.4,
            mean_net_return: 0.012,
            point_in_time: true,
            includes_delisted: true,
        }
    }

    fn good_paper() -> PaperResult {
        PaperResult { weeks: 28, signals: 14, mean_net_return: 0.004 }
    }

    fn rule() -> Rule {
        Rule::new("kundchock", "Supplier prices lag news about a large customer", 1).unwrap()
    }

    #[test]
    fn full_path_through_all_four_gates() {
        let mut r = rule();
        assert_eq!(r.stage, Stage::Hypothesis);
        r.record_backtest(good_backtest()).unwrap();
        assert_eq!(r.stage, Stage::Backtested);
        r.start_paper().unwrap();
        r.record_paper(good_paper()).unwrap();
        r.approve("Rasmus").unwrap();
        assert_eq!(r.stage, Stage::Approved);
        assert_eq!(r.approved_by.as_deref(), Some("Rasmus"));
        assert_eq!(r.variants_tested, 1);
    }

    #[test]
    fn hypothesis_must_be_written_down() {
        assert_eq!(Rule::new("x", "  ", 1), Err(GateError::EmptyHypothesis));
        assert_eq!(Rule::new("", "text", 1), Err(GateError::EmptyHypothesis));
    }

    #[test]
    fn weak_backtest_fails_and_lists_every_reason() {
        let mut r = rule();
        let err = r
            .record_backtest(BacktestResult {
                observations: 12,
                t_value: 2.1,
                mean_net_return: -0.001,
                point_in_time: false,
                includes_delisted: false,
            })
            .unwrap_err();
        match err {
            GateError::NotPassed(reasons) => assert_eq!(reasons.len(), 5, "{reasons:?}"),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(r.stage, Stage::Hypothesis);
        assert_eq!(r.variants_tested, 1);
        assert!(r.backtest.is_some(), "the failed result is kept on record");
    }

    #[test]
    fn t_value_must_be_strictly_above_three() {
        let mut r = rule();
        let mut b = good_backtest();
        b.t_value = 3.0;
        assert!(r.record_backtest(b).is_err());
        let mut b = good_backtest();
        b.t_value = f64::NAN;
        assert!(r.record_backtest(b).is_err());
        assert_eq!(r.variants_tested, 2);
        r.record_backtest(good_backtest()).unwrap();
        assert_eq!(r.variants_tested, 3);
    }

    #[test]
    fn gates_cannot_be_skipped() {
        let mut r = rule();
        assert_eq!(r.start_paper(), Err(GateError::WrongStage { at: Stage::Hypothesis, needs: Stage::Backtested }));
        assert!(matches!(r.approve("Rasmus"), Err(GateError::WrongStage { .. })));
        assert!(matches!(r.record_paper(good_paper()), Err(GateError::WrongStage { .. })));
        r.record_backtest(good_backtest()).unwrap();
        assert!(matches!(r.approve("Rasmus"), Err(GateError::WrongStage { .. })));
        assert!(matches!(r.record_backtest(good_backtest()), Err(GateError::WrongStage { .. })));
    }

    #[test]
    fn approval_needs_time_result_and_a_name() {
        let mut r = rule();
        r.record_backtest(good_backtest()).unwrap();
        r.start_paper().unwrap();
        assert!(matches!(r.approve("Rasmus"), Err(GateError::NotPassed(_))), "no paper result yet");

        r.record_paper(PaperResult { weeks: 27, signals: 9, mean_net_return: 0.01 }).unwrap();
        assert!(matches!(r.approve("Rasmus"), Err(GateError::NotPassed(_))), "one week short");

        r.record_paper(PaperResult { weeks: 40, signals: 9, mean_net_return: -0.01 }).unwrap();
        assert!(matches!(r.approve("Rasmus"), Err(GateError::NotPassed(_))), "lost money on paper");

        r.record_paper(good_paper()).unwrap();
        assert_eq!(r.approve("  "), Err(GateError::NoApprover));
        assert_eq!(r.stage, Stage::Paper);
        r.approve("Rasmus").unwrap();
    }

    #[test]
    fn retired_rule_stays_retired() {
        let mut r = rule();
        r.retire("forward results did not match the historical test");
        assert_eq!(r.stage, Stage::Retired);
        assert!(r.record_backtest(good_backtest()).is_err());
        assert!(r.start_paper().is_err());
    }

    #[test]
    fn revision_starts_over_but_remembers_attempts() {
        let mut r = rule();
        let _ = r.record_backtest(BacktestResult { t_value: 1.0, ..good_backtest() });
        let v2 = r.revise("Same idea, only customers above 20 percent", 99).unwrap();
        assert_eq!((v2.version, v2.stage, v2.variants_tested), (2, Stage::Hypothesis, 1));
        assert!(v2.backtest.is_none());
    }
}
