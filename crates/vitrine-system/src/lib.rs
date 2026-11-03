//! System metrics for Vitrine 2: a synchronous [`Sampler`] and a background
//! [`Collector`] that samples each metric at its own interval.

pub mod collector;
pub mod metric;
pub mod power;
pub mod sampler;

pub use collector::{Collector, Scheduler};
pub use metric::{Metric, UnknownMetric};
pub use sampler::Sampler;
