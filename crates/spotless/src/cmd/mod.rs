//! One module per subcommand. Each owns its own presentation; the shared
//! formatting primitives live in [`crate::ui`].

pub mod apps;
pub mod clean;
pub mod dev;
pub mod du;
pub mod dupes;
pub mod orphans;
pub mod rules;
pub mod scan;
pub mod trash;
