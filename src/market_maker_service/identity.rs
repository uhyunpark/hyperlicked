use std::collections::HashSet;

use anyhow::Result;
use sha2::{Digest, Sha256};

use crate::app::market_maker::{Intensity, StrategyType};
use crate::crypto::Signer;

const DEV_KEY_DOMAIN: &[u8] = b"HYPERLICKED-DEV-MM-ECDSA-V1\0";

/// A deterministic dev identity. The private key is intentionally not
/// exposed by this service type and is never included in logs.
pub struct DevIdentity {
    pub(crate) signer: Signer,
    address: String,
}

impl DevIdentity {
    pub fn address(&self) -> &str {
        &self.address
    }
}

/// Derive development signers from a domain-separated seed.
pub fn derive_dev_identities(seed: u64, intensity: Intensity) -> Result<Vec<DevIdentity>> {
    let mut identities = Vec::new();
    let mut addresses = HashSet::new();
    for (strategy_index, _) in StrategyType::all().iter().enumerate() {
        for account_index in 0..intensity.accounts_per_strategy() {
            let mut retry = 0u32;
            loop {
                let mut hasher = Sha256::new();
                hasher.update(DEV_KEY_DOMAIN);
                hasher.update(seed.to_le_bytes());
                hasher.update((strategy_index as u32).to_le_bytes());
                hasher.update((account_index as u32).to_le_bytes());
                hasher.update(retry.to_le_bytes());
                let key: [u8; 32] = hasher.finalize().into();
                retry = retry.saturating_add(1);
                let Ok(signer) = Signer::from_bytes(&key) else {
                    continue;
                };
                let address = format!("{:?}", signer.address());
                if addresses.insert(address.to_ascii_lowercase()) {
                    identities.push(DevIdentity { signer, address });
                    break;
                }
            }
        }
    }
    Ok(identities)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_identities_are_unique_and_stable() {
        let first = derive_dev_identities(7, Intensity::Low).unwrap();
        let second = derive_dev_identities(7, Intensity::Low).unwrap();
        assert_eq!(first.len(), 12);
        assert_eq!(
            first.iter().map(DevIdentity::address).collect::<Vec<_>>(),
            second.iter().map(DevIdentity::address).collect::<Vec<_>>()
        );
        let unique = first
            .iter()
            .map(|identity| identity.address().to_ascii_lowercase())
            .collect::<HashSet<_>>();
        assert_eq!(unique.len(), first.len());
    }
}
