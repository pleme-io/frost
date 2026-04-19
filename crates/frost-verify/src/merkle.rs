//! Layered BLAKE3 Merkle attestation compatible with pleme-io's tameshi.
//!
//! The convention matches `tameshi::merkle` exactly:
//!
//! - **Leaf**:          `BLAKE3(0x00 || entry_content_hash)`
//! - **Internal node**: `BLAKE3(0x01 || left || right)`
//!
//! This is the RFC 9162 / Certificate Transparency domain-separation
//! convention. Keeping it identical means a signed root produced here
//! can be gated by `inshou` / `sekiban` without translation.
//!
//! Composition for a shell config:
//!
//! 1. Each entry's file content hash is already BLAKE3 (hex in the manifest).
//! 2. Within a [`ConfigLayer`], entries are sorted by `(order, path)`
//!    for determinism, then folded into a layer Merkle root.
//! 3. Layer roots are sorted by `ConfigLayer` variant order and folded
//!    into the manifest root.

use crate::manifest::{ConfigLayer, ManifestEntry};

/// Raw 32-byte BLAKE3 digest.
pub type Digest = [u8; 32];

/// Apply leaf domain separation: `BLAKE3(0x00 || data)`.
#[inline]
#[must_use]
pub fn leaf_hash(content_hash: &Digest) -> Digest {
    let mut buf = [0u8; 33];
    buf[0] = 0x00;
    buf[1..].copy_from_slice(content_hash);
    blake3::hash(&buf).into()
}

/// Apply internal-node domain separation: `BLAKE3(0x01 || left || right)`.
#[inline]
#[must_use]
pub fn internal_hash(left: &Digest, right: &Digest) -> Digest {
    let mut buf = [0u8; 65];
    buf[0] = 0x01;
    buf[1..33].copy_from_slice(left);
    buf[33..].copy_from_slice(right);
    blake3::hash(&buf).into()
}

/// Fold a slice of leaf digests into a single Merkle root.
///
/// Uses `rs_merkle`-compatible odd-node handling: an odd-count level
/// propagates the last node unchanged up one level rather than duplicating it.
/// This matches tameshi's `Blake3Algorithm::concat_and_hash` where `right == None`.
#[must_use]
pub fn fold_merkle(leaves: &[Digest]) -> Option<Digest> {
    if leaves.is_empty() {
        return None;
    }
    if leaves.len() == 1 {
        return Some(leaves[0]);
    }
    let mut level: Vec<Digest> = leaves.to_vec();
    while level.len() > 1 {
        let mut next = Vec::with_capacity(level.len().div_ceil(2));
        let mut i = 0;
        while i < level.len() {
            if i + 1 < level.len() {
                next.push(internal_hash(&level[i], &level[i + 1]));
                i += 2;
            } else {
                next.push(level[i]);
                i += 1;
            }
        }
        level = next;
    }
    Some(level[0])
}

/// Decode a hex string (produced by BLAKE3 `to_hex`) into a [`Digest`].
pub fn decode_hex(hex: &str) -> Result<Digest, String> {
    if hex.len() != 64 {
        return Err(format!(
            "expected 64-hex-char BLAKE3 digest, got {} chars",
            hex.len()
        ));
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        let s = &hex[i * 2..i * 2 + 2];
        *byte = u8::from_str_radix(s, 16).map_err(|e| format!("hex decode: {e}"))?;
    }
    Ok(out)
}

/// Hex-encode a digest.
#[must_use]
pub fn encode_hex(digest: &Digest) -> String {
    let mut out = String::with_capacity(64);
    for b in digest {
        use std::fmt::Write;
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Per-layer Merkle root, hex-encoded, in canonical layer order.
#[derive(Debug, Clone)]
pub struct LayerRoot {
    pub layer: ConfigLayer,
    pub root: Digest,
    pub entry_count: usize,
}

/// Compute per-layer roots from manifest entries.
///
/// Entries within a layer are sorted by `(order, path)` so the root is
/// independent of input ordering.
pub fn compute_layer_roots(entries: &[ManifestEntry]) -> Result<Vec<LayerRoot>, String> {
    use std::collections::BTreeMap;

    let mut by_layer: BTreeMap<ConfigLayer, Vec<&ManifestEntry>> = BTreeMap::new();
    for e in entries {
        by_layer.entry(e.layer()).or_default().push(e);
    }

    let mut out = Vec::with_capacity(by_layer.len());
    for (layer, mut items) in by_layer {
        items.sort_by(|a, b| a.order.cmp(&b.order).then_with(|| a.path.cmp(&b.path)));
        let leaves: Vec<Digest> = items
            .iter()
            .map(|e| {
                decode_hex(&e.blake3)
                    .map(|d| leaf_hash(&d))
                    .map_err(|err| format!("{}: {err}", e.path))
            })
            .collect::<Result<_, _>>()?;
        let root = fold_merkle(&leaves).ok_or_else(|| format!("layer {layer} has no entries"))?;
        out.push(LayerRoot {
            layer,
            root,
            entry_count: items.len(),
        });
    }
    Ok(out)
}

/// Compute the manifest root: Merkle root over per-layer roots.
///
/// Layer roots are taken in `ConfigLayer` variant order (already guaranteed
/// by [`compute_layer_roots`]'s `BTreeMap`).
pub fn compute_manifest_root(entries: &[ManifestEntry]) -> Result<Digest, String> {
    let layer_roots = compute_layer_roots(entries)?;
    if layer_roots.is_empty() {
        return Err("cannot compute root: manifest has no entries".into());
    }
    let leaves: Vec<Digest> = layer_roots.iter().map(|lr| leaf_hash(&lr.root)).collect();
    fold_merkle(&leaves).ok_or_else(|| "empty layer set".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{EntryKind, Phase};

    fn entry(phase: Phase, kind: EntryKind, order: u32, path: &str, hash: &str) -> ManifestEntry {
        ManifestEntry {
            phase,
            order,
            path: path.into(),
            kind,
            blake3: hash.into(),
            name: None,
            priority: None,
            deferred: false,
        }
    }

    fn h(s: &str) -> String {
        encode_hex(&blake3::hash(s.as_bytes()).into())
    }

    #[test]
    fn leaf_and_internal_are_distinct_domains() {
        let z = [0u8; 32];
        assert_ne!(leaf_hash(&z), internal_hash(&z, &z));
    }

    #[test]
    fn fold_empty_is_none() {
        assert!(fold_merkle(&[]).is_none());
    }

    #[test]
    fn fold_single_leaf_is_itself() {
        let x = [7u8; 32];
        assert_eq!(fold_merkle(&[x]), Some(x));
    }

    #[test]
    fn fold_pair_matches_internal_hash() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        assert_eq!(fold_merkle(&[a, b]), Some(internal_hash(&a, &b)));
    }

    #[test]
    fn fold_odd_count_propagates_last() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let c = [3u8; 32];
        // Level 1: [hash(a,b), c]  — c has no sibling, propagates up.
        // Level 2: [hash(hash(a,b), c)]
        let l1 = internal_hash(&a, &b);
        let expected = internal_hash(&l1, &c);
        assert_eq!(fold_merkle(&[a, b, c]), Some(expected));
    }

    #[test]
    fn hex_roundtrip() {
        let d = blake3::hash(b"frost").into();
        let hex = encode_hex(&d);
        assert_eq!(hex.len(), 64);
        assert_eq!(decode_hex(&hex).unwrap(), d);
    }

    #[test]
    fn decode_hex_rejects_wrong_length() {
        assert!(decode_hex("abc").is_err());
    }

    #[test]
    fn manifest_root_is_deterministic() {
        let entries = vec![
            entry(Phase::Zshenv, EntryKind::EnvInit, 0, "~/.zshenv", &h("a")),
            entry(Phase::Zshrc, EntryKind::Group, 1, "g/x.zsh", &h("b")),
            entry(Phase::Zshrc, EntryKind::Plugin, 2, "p/y.zsh", &h("c")),
        ];
        let root1 = compute_manifest_root(&entries).unwrap();

        // Shuffle input order — layer-internal sort + layer Ord should make
        // the root identical.
        let mut shuffled = entries.clone();
        shuffled.reverse();
        let root2 = compute_manifest_root(&shuffled).unwrap();
        assert_eq!(root1, root2);
    }

    #[test]
    fn manifest_root_changes_on_content_change() {
        let mut entries = vec![
            entry(Phase::Zshenv, EntryKind::EnvInit, 0, "~/.zshenv", &h("a")),
            entry(Phase::Zshrc, EntryKind::Group, 1, "g/x.zsh", &h("b")),
        ];
        let root1 = compute_manifest_root(&entries).unwrap();
        entries[1].blake3 = h("b-tampered");
        let root2 = compute_manifest_root(&entries).unwrap();
        assert_ne!(root1, root2);
    }

    #[test]
    fn layer_roots_appear_in_canonical_order() {
        let entries = vec![
            // Deliberately scrambled input
            entry(Phase::Zshrc, EntryKind::Plugin, 2, "p.zsh", &h("p")),
            entry(Phase::Zshenv, EntryKind::EnvInit, 0, "env", &h("e")),
            entry(Phase::Zshrc, EntryKind::Group, 1, "g.zsh", &h("g")),
        ];
        let layer_roots = compute_layer_roots(&entries).unwrap();
        assert_eq!(layer_roots[0].layer, ConfigLayer::ZshenvCore);
        assert_eq!(layer_roots[1].layer, ConfigLayer::ZshrcGroups);
        assert_eq!(layer_roots[2].layer, ConfigLayer::ZshrcPlugins);
    }
}
