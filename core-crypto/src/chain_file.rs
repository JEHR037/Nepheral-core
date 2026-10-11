// SPDX-License-Identifier: BUSL-1.1
//! The on-disk chain file's header.
//!
//! This lives in `core-crypto` rather than in the node because more than one
//! process reads the file. A reader that guessed the layout, or carried its
//! own idea of which generation it understood, could replay a chain it does
//! not actually agree with and act on the result — so the format and the
//! generation are defined once, here, and everything that touches the file
//! shares them.

/// Marks the file as ours, so a truncated or unrelated file is rejected rather
/// than parsed into an empty chain that looks like a fresh network.
pub const CHAIN_FILE_MAGIC: &[u8; 8] = b"NEPHCHN\0";

/// Incremented whenever a change makes earlier history reconcile to a
/// different state root. A node reading a file from another generation
/// discards it and starts fresh rather than serving state its peers will
/// disagree with.
///
/// v4: `DEFAULT_SHARES_PER_CREDIT` dropped 1500 -> 1000.
/// v5: `NaiStorage` gained `sale_oracle`.
/// v6: the Market pallet was added and joined the state root.
/// v7: Contributions began recording first-contribution times, and the Market
///     gained claims.
/// v11: The emission controller measures its window in block timestamps
///      instead of block counts, which changes every ratio it computes.
pub const CHAIN_GENERATION: u32 = 11;

const HEADER_LEN: usize = CHAIN_FILE_MAGIC.len() + 4;

/// Wraps serialized blocks with the magic and generation.
pub fn with_header(blocks: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(blocks.len() + HEADER_LEN);
    out.extend_from_slice(CHAIN_FILE_MAGIC);
    out.extend_from_slice(&CHAIN_GENERATION.to_be_bytes());
    out.extend_from_slice(blocks);
    out
}

/// Returns the block bytes, or `None` when the file is not ours or belongs to
/// a different generation. Both cases mean the same thing to a caller: this
/// file cannot be trusted as the current chain.
pub fn strip_header(data: &[u8]) -> Option<&[u8]> {
    let (magic, rest) = data.split_at_checked(CHAIN_FILE_MAGIC.len())?;
    if magic != CHAIN_FILE_MAGIC {
        return None;
    }
    let (generation_bytes, blocks) = rest.split_at_checked(4)?;
    let generation = u32::from_be_bytes(generation_bytes.try_into().ok()?);
    (generation == CHAIN_GENERATION).then_some(blocks)
}

/// The generation a file was written by, whatever it is. Lets a reader explain
/// a mismatch instead of only reporting that the file was unusable.
pub fn generation_of(data: &[u8]) -> Option<u32> {
    let (magic, rest) = data.split_at_checked(CHAIN_FILE_MAGIC.len())?;
    if magic != CHAIN_FILE_MAGIC {
        return None;
    }
    let (generation_bytes, _) = rest.split_at_checked(4)?;
    Some(u32::from_be_bytes(generation_bytes.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_header_round_trips() {
        let blocks = b"some blocks";
        let framed = with_header(blocks);
        assert_eq!(strip_header(&framed), Some(&blocks[..]));
    }

    #[test]
    fn an_empty_chain_still_carries_a_header() {
        let framed = with_header(&[]);
        assert_eq!(framed.len(), HEADER_LEN);
        assert_eq!(strip_header(&framed), Some(&[][..]));
    }

    #[test]
    fn a_foreign_file_is_rejected() {
        assert_eq!(strip_header(b"not a chain file at all"), None);
        assert_eq!(generation_of(b"not a chain file at all"), None);
    }

    #[test]
    fn a_truncated_file_is_rejected_rather_than_read_as_empty() {
        // Reading a half-written file as an empty chain would look exactly
        // like a fresh network, which is the worst possible misreading.
        assert_eq!(strip_header(b"NEPH"), None);
        assert_eq!(strip_header(CHAIN_FILE_MAGIC), None);
        let mut short = CHAIN_FILE_MAGIC.to_vec();
        short.extend_from_slice(&[0, 0]);
        assert_eq!(strip_header(&short), None);
    }

    #[test]
    fn another_generation_is_rejected_but_still_identifiable() {
        let mut other = CHAIN_FILE_MAGIC.to_vec();
        other.extend_from_slice(&(CHAIN_GENERATION - 1).to_be_bytes());
        other.extend_from_slice(b"blocks");
        assert_eq!(strip_header(&other), None);
        assert_eq!(generation_of(&other), Some(CHAIN_GENERATION - 1));
    }
}
