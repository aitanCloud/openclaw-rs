/// Budget amount in integer cents. NEVER use floats for money.
pub type BudgetCents = i64;

/// Basis points. 10000 = 100%. Used for contingency, percentages.
pub type BasisPoints = u16;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_cents_is_i64() {
        let amount: BudgetCents = -500;
        assert_eq!(amount, -500_i64);
    }

    #[test]
    fn basis_points_is_u16() {
        let pct: BasisPoints = 10_000; // 100%
        assert_eq!(pct, 10_000_u16);
    }

    #[test]
    fn basis_points_half() {
        let half: BasisPoints = 5_000; // 50%
        assert_eq!(half, 5_000_u16);
    }
}
