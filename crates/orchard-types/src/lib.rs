//! Orchard 3.0 type system + static checker (`orch check`).

pub mod check;
pub mod types;

pub use check::{check, check_source};
pub use types::{assignable, from_typeref, to_json_schema, unify, EnumType, RecordType, Type};
