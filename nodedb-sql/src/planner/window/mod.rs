// SPDX-License-Identifier: Apache-2.0

//! Window function planning: `WINDOW`-clause resolution, frame conversion,
//! and extraction of `<func>() OVER (...)` specs from a SELECT projection.

mod extract;
mod frame;
mod named;

pub use extract::extract_window_functions;
