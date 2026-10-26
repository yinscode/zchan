//! Ironwood notes a party can spend: found by trial decryption, witnessed from the
//! node's tree state.
//!
//! No wallet tree is kept. A note's witness starts from the commitment tree frontier of
//! the block before it (`z_gettreestate`), adds the commitments of its own block up to
//! and including it, then every commitment after it up to the anchor height. The
//! result's root must equal the node's root at that height.

use incrementalmerkletree::witness::IncrementalWitness;
use orchard::{
    keys::{FullViewingKey, PreparedIncomingViewingKey, Scope},
    note::{ExtractedNoteCommitment, Nullifier},
    note_encryption::IronwoodDomain,
    tree::{MerkleHashOrchard, MerklePath},
    Note,
};
use serde_json::json;
use zcash_note_encryption::try_note_decryption;
use zcash_primitives::{merkle_tree::read_commitment_tree, transaction::Transaction};
use zcash_protocol::consensus::BranchId;

use crate::{node::Node, tx::IronwoodSpend, Error, Result};

/// A received note.
#[derive(Clone, Debug)]
pub struct OwnedNote {
    pub note: Note,
    pub height: u32,
    /// Internal byte order.
    pub txid: [u8; 32],
    pub action: usize,
}

impl OwnedNote {
    pub fn value(&self) -> u64 {
        self.note.value().inner()
    }
}

/// Parses a raw v5/v6 transaction (its consensus branch id is bytes 8..12).
pub fn parse_tx(raw: &[u8]) -> Option<Transaction> {
    let branch = BranchId::try_from(u32::from_le_bytes(raw.get(8..12)?.try_into().ok()?)).ok()?;
    Transaction::read(raw, branch).ok()
}

type Act = orchard::Action<orchard::primitives::redpallas::Signature<orchard::primitives::redpallas::SpendAuth>>;

/// Every Ironwood action of a block, in tree order.
async fn block_actions(node: &Node, h: u32) -> Result<Vec<([u8; 32], usize, Act)>> {
    let block = node.block(h).await?;
    let mut out = vec![];
    for txid in block["tx"].as_array().cloned().unwrap_or_default() {
        let raw = node.call("getrawtransaction", json!([txid.as_str().unwrap_or_default(), 0])).await?;
        let raw = hex::decode(raw.as_str().unwrap_or_default()).map_err(|e| Error::Rpc(e.to_string()))?;
        let Some(t) = parse_tx(&raw) else { continue };
        let id = *t.txid().as_ref();
        if let Some(b) = t.ironwood_bundle() {
            for (i, a) in b.actions().iter().enumerate() {
                out.push((id, i, a.clone()));
            }
        }
    }
    Ok(out)
}

/// Unspent notes to `fvk` (both scopes) received in blocks `from..=to`.
pub async fn unspent(node: &Node, fvk: &FullViewingKey, from: u32, to: u32) -> Result<Vec<OwnedNote>> {
    let ivks: Vec<PreparedIncomingViewingKey> = [Scope::External, Scope::Internal]
        .iter()
        .map(|s| PreparedIncomingViewingKey::new(&fvk.to_ivk(*s)))
        .collect();
    let mut found: Vec<(OwnedNote, Nullifier)> = vec![];
    let mut spent: Vec<Nullifier> = vec![];
    for h in from..=to {
        for (txid, i, a) in block_actions(node, h).await? {
            spent.push(*a.nullifier());
            let domain = IronwoodDomain::for_action(&a);
            for ivk in &ivks {
                if let Some((note, _, _)) = try_note_decryption(&domain, ivk, &a) {
                    let nf = note.nullifier(fvk);
                    found.push((OwnedNote { note, height: h, txid, action: i }, nf));
                    break;
                }
            }
        }
    }
    Ok(found.into_iter().filter(|(_, nf)| !spent.contains(nf)).map(|(n, _)| n).collect())
}

fn tree_state(v: &serde_json::Value) -> Result<incrementalmerkletree::frontier::CommitmentTree<MerkleHashOrchard, 32>> {
    let hex_state = v["ironwood"]["commitments"]["finalState"].as_str().unwrap_or("000000");
    let bytes = hex::decode(hex_state).map_err(|e| Error::Rpc(format!("finalState: {e}")))?;
    read_commitment_tree::<MerkleHashOrchard, _, 32>(&bytes[..]).map_err(|e| Error::Rpc(format!("finalState: {e}")))
}

fn leaf(cmx: &[u8; 32]) -> Result<MerkleHashOrchard> {
    Option::from(ExtractedNoteCommitment::from_bytes(cmx))
        .map(|c| MerkleHashOrchard::from_cmx(&c))
        .ok_or(Error::Build("cmx".into()))
}

/// The note's Merkle path to the tree root at `anchor_height`, and that root.
pub async fn witness(node: &Node, n: &OwnedNote, anchor_height: u32) -> Result<(MerklePath, orchard::Anchor)> {
    let before = node.call("z_gettreestate", json!([(n.height - 1).to_string()])).await?;
    let mut tree = tree_state(&before)?;
    let mut w: Option<IncrementalWitness<MerkleHashOrchard, 32>> = None;
    for h in n.height..=anchor_height {
        for (txid, i, a) in block_actions(node, h).await? {
            let l = leaf(&a.cmx().to_bytes())?;
            match &mut w {
                Some(w) => w.append(l).map_err(|_| Error::Build("tree full".into()))?,
                None => {
                    tree.append(l).map_err(|_| Error::Build("tree full".into()))?;
                    if (txid, i) == (n.txid, n.action) {
                        w = IncrementalWitness::from_tree(tree.clone());
                    }
                }
            }
        }
    }
    let w = w.ok_or(Error::Build("note not found in its block".into()))?;
    let anchor = node.ironwood_anchor(anchor_height).await?;
    if orchard::Anchor::from(w.root()) != anchor {
        return Err(Error::Build(format!("witness root differs from the node's at {anchor_height}")));
    }
    let path = w.path().ok_or(Error::Build("witness path".into()))?;
    Ok((MerklePath::from(path), anchor))
}

/// Spends of `notes` (all to `fvk`) against one anchor at `anchor_height`.
pub async fn spends(node: &Node, fvk: &FullViewingKey, notes: &[OwnedNote], anchor_height: u32) -> Result<(Vec<IronwoodSpend>, orchard::Anchor)> {
    let mut out = vec![];
    let mut anchor = None;
    for n in notes {
        let (path, a) = witness(node, n, anchor_height).await?;
        anchor = Some(a);
        out.push(IronwoodSpend { fvk: fvk.clone(), note: n.note, path });
    }
    Ok((out, anchor.ok_or(Error::Build("no notes".into()))?))
}

/// The fewest notes, largest first, that cover `need`.
pub fn select(mut notes: Vec<OwnedNote>, need: u64) -> Option<Vec<OwnedNote>> {
    notes.sort_by_key(|n| std::cmp::Reverse(n.value()));
    let mut got = 0;
    let mut out = vec![];
    for n in notes {
        if got >= need {
            break;
        }
        got += n.value();
        out.push(n);
    }
    (got >= need).then_some(out)
}

// ---- Persistent scanner -----------------------------------------------------------------

use incrementalmerkletree::frontier::CommitmentTree;
use orchard::note::{NoteVersion, RandomSeed, Rho};
use orchard::value::NoteValue;
use zcash_primitives::merkle_tree::{read_incremental_witness, write_commitment_tree, write_incremental_witness};

type Tree = CommitmentTree<MerkleHashOrchard, 32>;
type Witness = IncrementalWitness<MerkleHashOrchard, 32>;

struct Tracked {
    owned: OwnedNote,
    nf: Nullifier,
    witness: Witness,
}

/// A wallet's shielded state, kept between runs: the Ironwood commitment tree frontier at
/// `scanned_to`, and every unspent note with its nullifier and an up-to-date witness.
///
/// `update` reads only blocks where the Ironwood tree grew: the node's tree size at two
/// heights tells whether anything happened between them, so a bisection over
/// `z_gettreestate` finds those blocks without reading the others. Every relevant block is
/// read once per wallet, and spending needs no further scan.
pub struct Scanner {
    pub scanned_to: u32,
    tree: Tree,
    notes: Vec<Tracked>,
}

async fn tree_at(node: &Node, h: u32) -> Result<Tree> {
    tree_state(&node.call("z_gettreestate", json!([h.to_string()])).await?)
}

fn version_byte(v: NoteVersion) -> u8 {
    match v {
        NoteVersion::V2 => 2,
        _ => 3,
    }
}

impl Scanner {
    /// A scanner whose history starts at `from` (nothing before it is looked at).
    pub async fn start(node: &Node, from: u32) -> Result<Self> {
        Ok(Self { scanned_to: from, tree: tree_at(node, from).await?, notes: vec![] })
    }

    /// Heights in `(self.scanned_to, to]` whose block grew the Ironwood tree.
    async fn grown(&self, node: &Node, to: u32) -> Result<Vec<u32>> {
        let mut out = vec![];
        let mut stack = vec![(self.scanned_to, self.tree.size(), to, tree_at(node, to).await?.size())];
        while let Some((lo, s_lo, hi, s_hi)) = stack.pop() {
            if s_lo == s_hi || hi <= lo {
                continue;
            }
            if hi == lo + 1 {
                out.push(hi);
                continue;
            }
            let mid = lo + (hi - lo) / 2;
            let s_mid = tree_at(node, mid).await?.size();
            stack.push((mid, s_mid, hi, s_hi));
            stack.push((lo, s_lo, mid, s_mid));
        }
        out.sort_unstable();
        Ok(out)
    }

    /// Reads every block up to `to` that touched the Ironwood tree: drops notes whose
    /// nullifier appears, advances every witness, and takes in new notes to `fvk`.
    pub async fn update(&mut self, node: &Node, fvk: &FullViewingKey, to: u32) -> Result<()> {
        if to <= self.scanned_to {
            return Ok(());
        }
        let ivks: Vec<PreparedIncomingViewingKey> = [Scope::External, Scope::Internal]
            .iter()
            .map(|s| PreparedIncomingViewingKey::new(&fvk.to_ivk(*s)))
            .collect();
        for h in self.grown(node, to).await? {
            for (txid, i, a) in block_actions(node, h).await? {
                self.notes.retain(|t| t.nf != *a.nullifier());
                let l = leaf(&a.cmx().to_bytes())?;
                self.tree.append(l).map_err(|_| Error::Build("tree full".into()))?;
                for t in &mut self.notes {
                    t.witness.append(l).map_err(|_| Error::Build("witness full".into()))?;
                }
                let domain = IronwoodDomain::for_action(&a);
                for ivk in &ivks {
                    if let Some((note, _, _)) = try_note_decryption(&domain, ivk, &a) {
                        let witness = IncrementalWitness::from_tree(self.tree.clone())
                            .ok_or(Error::Build("witness of an empty tree".into()))?;
                        let nf = note.nullifier(fvk);
                        self.notes.push(Tracked { owned: OwnedNote { note, height: h, txid, action: i }, nf, witness });
                        break;
                    }
                }
            }
        }
        if orchard::Anchor::from(self.tree.root()) != node.ironwood_anchor(to).await? {
            return Err(Error::Build(format!("scanned tree differs from the node's at {to}")));
        }
        self.scanned_to = to;
        Ok(())
    }

    pub fn unspent(&self) -> Vec<OwnedNote> {
        self.notes.iter().map(|t| t.owned.clone()).collect()
    }

    /// Spends of `picks` (notes of this scanner, to `fvk`) against the scanned tree.
    pub fn spends(&self, fvk: &FullViewingKey, picks: &[OwnedNote]) -> Result<(Vec<IronwoodSpend>, orchard::Anchor)> {
        let mut out = vec![];
        for p in picks {
            let t = self
                .notes
                .iter()
                .find(|t| (t.owned.txid, t.owned.action) == (p.txid, p.action))
                .ok_or(Error::Build("note not tracked".into()))?;
            let path = t.witness.path().ok_or(Error::Build("witness path".into()))?;
            out.push(IronwoodSpend { fvk: fvk.clone(), note: t.owned.note, path: MerklePath::from(path) });
        }
        Ok((out, orchard::Anchor::from(self.tree.root())))
    }

    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let io = |e: std::io::Error| Error::Build(format!("scanner: {e}"));
        let mut b = self.scanned_to.to_le_bytes().to_vec();
        write_commitment_tree(&self.tree, &mut b).map_err(io)?;
        b.extend((self.notes.len() as u32).to_le_bytes());
        for t in &self.notes {
            let n = &t.owned.note;
            b.extend(n.recipient().to_raw_address_bytes());
            b.extend(n.value().inner().to_le_bytes());
            b.extend(n.rho().to_bytes());
            b.extend(n.rseed().as_bytes());
            b.push(version_byte(n.version()));
            b.extend(t.owned.height.to_le_bytes());
            b.extend(t.owned.txid);
            b.extend((t.owned.action as u32).to_le_bytes());
            b.extend(t.nf.to_bytes());
            write_incremental_witness(&t.witness, &mut b).map_err(io)?;
        }
        Ok(b)
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let bad = |m: &str| Error::Build(format!("scanner bytes: {m}"));
        let mut r = std::io::Cursor::new(bytes);
        let scanned_to = u32::from_le_bytes(take(&mut r)?);
        let tree = read_commitment_tree::<MerkleHashOrchard, _, 32>(&mut r).map_err(|e| bad(&e.to_string()))?;
        let count = u32::from_le_bytes(take(&mut r)?);
        let mut notes = vec![];
        for _ in 0..count {
            let recipient: [u8; 43] = take(&mut r)?;
            let value = u64::from_le_bytes(take(&mut r)?);
            let rho: [u8; 32] = take(&mut r)?;
            let rseed: [u8; 32] = take(&mut r)?;
            let version = if take::<1>(&mut r)?[0] == 2 { NoteVersion::V2 } else { NoteVersion::V3 };
            let height = u32::from_le_bytes(take(&mut r)?);
            let txid: [u8; 32] = take(&mut r)?;
            let action = u32::from_le_bytes(take(&mut r)?) as usize;
            let nf: [u8; 32] = take(&mut r)?;
            let witness = read_incremental_witness::<MerkleHashOrchard, _, 32>(&mut r).map_err(|e| bad(&e.to_string()))?;
            let recipient = Option::from(orchard::Address::from_raw_address_bytes(&recipient)).ok_or_else(|| bad("recipient"))?;
            let rho = Option::<Rho>::from(Rho::from_bytes(&rho)).ok_or_else(|| bad("rho"))?;
            let rseed = Option::<RandomSeed>::from(RandomSeed::from_bytes(rseed, &rho)).ok_or_else(|| bad("rseed"))?;
            let note = Option::<Note>::from(Note::from_parts(recipient, NoteValue::from_raw(value), rho, rseed, version))
                .ok_or_else(|| bad("note"))?;
            let nf = Option::<Nullifier>::from(Nullifier::from_bytes(&nf)).ok_or_else(|| bad("nullifier"))?;
            notes.push(Tracked { owned: OwnedNote { note, height, txid, action }, nf, witness });
        }
        Ok(Self { scanned_to, tree, notes })
    }
}

fn take<const N: usize>(r: &mut std::io::Cursor<&[u8]>) -> Result<[u8; N]> {
    let mut v = [0u8; N];
    std::io::Read::read_exact(r, &mut v).map_err(|_| Error::Build("scanner bytes: short".into()))?;
    Ok(v)
}
