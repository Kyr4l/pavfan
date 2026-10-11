// SPDX-License-Identifier: GPL-3.0-or-later
//! Refuse to touch the EC on any machine the register map was not verified on.

use std::fs;
use std::path::Path;

use crate::error::{Error, Result};

pub const PRODUCT: &str = "HP Pavilion Gaming Laptop 15-dk1xxx";
pub const BOARD: &str = "8742";
/// BIOS versions on which the register map was verified on real hardware.
pub const VERIFIED_BIOS: &[&str] = &["F.50"];
pub const DMI_DIR: &str = "/sys/class/dmi/id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub product: String,
    pub board: String,
    pub bios: String,
}

pub fn read_identity(dir: &Path) -> Result<Identity> {
    let read = |name: &str| -> Result<String> {
        fs::read_to_string(dir.join(name))
            .map(|s| s.trim().to_owned())
            .map_err(|e| Error::io(format!("reading DMI {name}"), e))
    };
    Ok(Identity {
        product: read("product_name")?,
        board: read("board_name")?,
        bios: read("bios_version")?,
    })
}

/// Outcome of the identity check.
#[derive(Debug, PartialEq, Eq)]
pub enum Compat {
    /// Exactly the verified machine and BIOS.
    Verified,
    /// Right machine, BIOS not verified; only allowed when explicitly requested.
    UnverifiedBios(String),
}

pub fn check(id: &Identity, allow_unverified_bios: bool) -> Result<Compat> {
    if id.product != PRODUCT || id.board != BOARD {
        return Err(Error::UnsupportedMachine(format!(
            "found \"{}\" / board \"{}\", need \"{PRODUCT}\" / board \"{BOARD}\"",
            id.product, id.board
        )));
    }
    if VERIFIED_BIOS.contains(&id.bios.as_str()) {
        return Ok(Compat::Verified);
    }
    if allow_unverified_bios {
        return Ok(Compat::UnverifiedBios(id.bios.clone()));
    }
    Err(Error::UnsupportedMachine(format!(
        "BIOS \"{}\" is not one of the verified versions {VERIFIED_BIOS:?}; \
         the EC register map may differ (override with --allow-unverified-bios at your own risk)",
        id.bios
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(product: &str, board: &str, bios: &str) -> Identity {
        Identity { product: product.into(), board: board.into(), bios: bios.into() }
    }

    #[test]
    fn accepts_verified_machine() {
        assert_eq!(check(&id(PRODUCT, BOARD, "F.50"), false).ok(), Some(Compat::Verified));
    }

    #[test]
    fn rejects_other_product_even_with_override() {
        assert!(check(&id("HP OMEN 15", BOARD, "F.50"), true).is_err());
    }

    #[test]
    fn rejects_other_board() {
        assert!(check(&id(PRODUCT, "8743", "F.50"), true).is_err());
    }

    #[test]
    fn unknown_bios_needs_explicit_override() {
        assert!(check(&id(PRODUCT, BOARD, "F.60"), false).is_err());
        assert_eq!(
            check(&id(PRODUCT, BOARD, "F.60"), true).ok(),
            Some(Compat::UnverifiedBios("F.60".into()))
        );
    }
}
