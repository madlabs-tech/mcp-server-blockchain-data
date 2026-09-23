//! SPL Token / Token-2022 helpers. Owner: `solana` (T1.S1). Used by payments, neobank, wallet.

use ems_domain::{DomainError, SolanaPubkey};

pub const TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";
pub const ASSOCIATED_TOKEN_PROGRAM: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";

/// ATA address, derived with the mint's token program (Token-2022 mints differ from legacy).
pub fn associated_token_address(
    _owner: &SolanaPubkey,
    _mint: &SolanaPubkey,
    _token_program: &SolanaPubkey,
) -> Result<SolanaPubkey, DomainError> {
    Err(DomainError::internal(
        "not implemented yet (T1.S1 spl::associated_token_address)",
    ))
}
