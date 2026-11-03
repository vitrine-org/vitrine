use std::str::FromStr;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Metric {
    System,
    Cpu,
    Memory,
    Battery,
    Drives,
    Processes,
    Network,
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown metric `{0}`")]
pub struct UnknownMetric(pub String);

impl Metric {
    pub const ALL: [Metric; 7] = [
        Metric::System,
        Metric::Cpu,
        Metric::Memory,
        Metric::Battery,
        Metric::Drives,
        Metric::Processes,
        Metric::Network,
    ];

    /// Lowercase name, which is also the key in the snapshot JSON.
    pub fn as_str(self) -> &'static str {
        match self {
            Metric::System => "system",
            Metric::Cpu => "cpu",
            Metric::Memory => "memory",
            Metric::Battery => "battery",
            Metric::Drives => "drives",
            Metric::Processes => "processes",
            Metric::Network => "network",
        }
    }
}

impl FromStr for Metric {
    type Err = UnknownMetric;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Metric::ALL
            .into_iter()
            .find(|m| m.as_str() == s)
            .ok_or_else(|| UnknownMetric(s.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for m in Metric::ALL {
            assert_eq!(Metric::from_str(m.as_str()), Ok(m));
        }
        assert_eq!(Metric::from_str("gpu"), Err(UnknownMetric("gpu".into())));
    }
}
