// SPDX-License-Identifier: GPL-3.0-or-later
//! Safe fan control for the HP Pavilion Gaming Laptop 15-dk1xxx (board 8742).
//!
//! Layering: `ec` (raw bytes) <- `controller` (the only writer, enforces the
//! safety invariants) <- the caller's control loop, fed by `sensors` and decided
//! by `governor` (pure logic over `curve`s).

#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)
)]

pub mod controller;
pub mod curve;
pub mod ec;
pub mod error;
pub mod governor;
pub mod machine;
pub mod runtime;
pub mod sensors;

pub use error::{Error, Result};
