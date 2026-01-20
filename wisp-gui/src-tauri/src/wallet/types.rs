#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeeType {
    Fixed,
    Percent,
}

impl std::fmt::Display for FeeType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeeType::Fixed => write!(f, "Fixed Amount"),
            FeeType::Percent => write!(f, "Percentage of Amount Sent"),
        }
    }
}
