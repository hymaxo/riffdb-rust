// Port of Any.h (unused in the original code base as well).
//
// The tagged union (AnyType + union) is a Rust enum.

#![allow(dead_code)]

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Any {
    /// AnyNull
    #[default]
    Null,
    Int(i64),
    Double(f64),
    Bool(bool),
    Str(String),
}
