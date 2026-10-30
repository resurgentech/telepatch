//! Ansible corpus loader.

pub mod loader;
pub mod task;
pub mod yaml;

pub use loader::{LoadError, Loader, find_roles_dir};
pub use task::{IncludeFrame, Loaded, Play, Skipped, Task};
