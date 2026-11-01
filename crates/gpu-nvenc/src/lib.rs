//! Własne FFI do NVENC (libnvidia-encode, przez dlopen) i CUDA oraz bezpieczna nakładka kodera H.264.
#![allow(clippy::manual_range_contains)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
pub mod api;
pub mod consts;
pub mod cuda;
pub mod dl;
pub mod encoder;
#[allow(non_camel_case_types, non_snake_case, non_upper_case_globals, dead_code, clippy::all, rustdoc::all, unpredictable_function_pointer_comparisons)]
pub mod sys;
