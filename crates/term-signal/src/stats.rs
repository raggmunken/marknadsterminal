//! The arithmetic behind the gates. Small on purpose, and tested against
//! hand calculated values.

use crate::Side;

pub fn mean(xs: &[f64]) -> Option<f64> {
    if xs.is_empty() {
        None
    } else {
        Some(xs.iter().sum::<f64>() / xs.len() as f64)
    }
}

/// Sample standard deviation (divides by n minus 1).
pub fn sample_std(xs: &[f64]) -> Option<f64> {
    if xs.len() < 2 {
        return None;
    }
    let m = mean(xs)?;
    let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (xs.len() - 1) as f64;
    Some(var.sqrt())
}

/// One sample t value of the mean against zero.
///
/// `None` when there are fewer than two observations or no variation,
/// because the value is undefined then. Note that this assumes independent
/// observations. Overlapping holding periods break that assumption and
/// make the value too high.
pub fn t_value(xs: &[f64]) -> Option<f64> {
    let sd = sample_std(xs)?;
    if sd == 0.0 {
        return None;
    }
    Some(mean(xs)? / (sd / (xs.len() as f64).sqrt()))
}

/// Return of one closed position after costs.
///
/// `round_trip_cost_bps` covers commission and spread for entry and exit
/// together, in basis points of the position. `None` if a price is not
/// positive.
pub fn net_return(side: Side, entry: f64, exit: f64, round_trip_cost_bps: f64) -> Option<f64> {
    if !(entry > 0.0 && exit > 0.0) || round_trip_cost_bps < 0.0 {
        return None;
    }
    let gross = match side {
        Side::Buy => exit / entry - 1.0,
        Side::Sell => 1.0 - exit / entry,
    };
    Some(gross - round_trip_cost_bps / 10_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn mean_and_std_by_hand() {
        let xs = [0.01, 0.02, 0.03];
        assert!(close(mean(&xs).unwrap(), 0.02));
        assert!(close(sample_std(&xs).unwrap(), 0.01));
        assert_eq!(mean(&[]), None);
        assert_eq!(sample_std(&[0.5]), None);
    }

    #[test]
    fn t_value_by_hand() {
        // mean 0.02, sd 0.01, n 3: 0.02 / (0.01 / sqrt(3)) = 2 * sqrt(3)
        assert!(close(t_value(&[0.01, 0.02, 0.03]).unwrap(), 2.0 * 3.0_f64.sqrt()));
        // Symmetric around zero gives zero.
        assert!(close(t_value(&[-0.01, 0.01]).unwrap(), 0.0));
    }

    #[test]
    fn t_value_is_undefined_without_variation_or_data() {
        assert_eq!(t_value(&[]), None);
        assert_eq!(t_value(&[0.02]), None);
        assert_eq!(t_value(&[0.02, 0.02, 0.02]), None);
    }

    #[test]
    fn net_return_long_and_short() {
        assert!(close(net_return(Side::Buy, 100.0, 110.0, 20.0).unwrap(), 0.098));
        assert!(close(net_return(Side::Sell, 100.0, 90.0, 20.0).unwrap(), 0.098));
        assert!(close(net_return(Side::Buy, 100.0, 100.0, 20.0).unwrap(), -0.002));
        assert!(close(net_return(Side::Sell, 100.0, 110.0, 0.0).unwrap(), -0.10));
    }

    #[test]
    fn net_return_rejects_bad_input() {
        assert_eq!(net_return(Side::Buy, 0.0, 10.0, 0.0), None);
        assert_eq!(net_return(Side::Buy, 10.0, -1.0, 0.0), None);
        assert_eq!(net_return(Side::Buy, 10.0, 11.0, -5.0), None);
        assert_eq!(net_return(Side::Buy, f64::NAN, 11.0, 0.0), None);
    }
}
