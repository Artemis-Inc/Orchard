//! Model providers behind the [`crate::traits::Provider`] trait.

pub mod mock;
pub mod remote;

pub use mock::MockProvider;
pub use remote::{get_provider, FallbackProvider};
