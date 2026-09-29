// Port of Any.h (unused in the original code base as well).

#![allow(dead_code)]

use libc::c_char;

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AnyType {
    Null,
    Int,
    Double,
    Bool,
    Str,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub union AnyValue {
    pub int: i64,
    pub double: f64,
    pub bool: bool,
    pub str: *mut c_char,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Any {
    pub ty: AnyType,
    pub value: AnyValue,
}

pub const ANY_NULL: Any = Any {
    ty: AnyType::Null,
    value: AnyValue { int: 0 },
};
