//! The snow itself, free of any platform: the simulation, its drifts and the geometry they use.
//! Derived from the platform-independent simulation in the upstream Snowhush project.

pub mod geometry;
pub mod halloween;
pub mod layer;
pub mod simulation;
pub mod snowmen;
pub mod text;
pub mod version;

#[cfg(test)]
mod tests;
