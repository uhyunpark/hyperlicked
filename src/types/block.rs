//! Block Type
//!
//! A block in the chain.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Certificate, Hash, Height, NodeId, View};

/// A block in the chain.
///
/// Blocks form a chain via `parent` hash. Each block has:
/// - `view`: The consensus round it was proposed in
/// - `height`: Position in committed chain (0 = genesis)
/// - `payload`: Serialized transactions
/// - `app_hash`: State root after executing this block
/// - `justify`: QC that justifies this block (proves parent was certified)
///
/// The `justify` field is NOT included in the block hash (it's metadata
/// about how we received the block, not part of the block identity).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub view: View,
    pub height: Height,
    pub parent: Hash,
    pub payload: Vec<u8>,
    pub proposer: NodeId,
    pub app_hash: Hash,
    pub timestamp: u64,
    /// QC that justifies this block (proves parent was certified).
    /// Not included in block hash - it's consensus metadata.
    #[serde(default)]
    pub justify: Option<Certificate>,
}

impl Block {
    /// Compute the hash of this block
    pub fn hash(&self) -> Hash {
        let mut hasher = Sha256::new();
        hasher.update(self.view.to_le_bytes());
        hasher.update(self.height.to_le_bytes());
        hasher.update(self.parent);
        hasher.update(&self.payload);
        hasher.update(self.proposer);
        hasher.update(self.app_hash);
        hasher.update(self.timestamp.to_le_bytes());
        hasher.finalize().into()
    }

    /// Create genesis block (height 0, no parent)
    pub fn genesis() -> Self {
        Self {
            view: 0,
            height: 0,
            parent: [0u8; 32],
            payload: vec![],
            proposer: [0u8; 32],
            app_hash: [0u8; 32],
            timestamp: 0,
            justify: None,
        }
    }
}
