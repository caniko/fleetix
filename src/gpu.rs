//! GPU route identities shared by topology validators and runtime consumers.

/// Generated from `lib/topology/GpuContract.pkl`, also consumed by Nix.
pub const CONTRACT_JSON: &str = include_str!("../lib/generated/gpu-contract.json");

/// Translate an exact stable PCI render-node alias to Mesa's PCI selector.
/// Device existence, driver support and access remain runtime checks.
pub fn pci_selector(node: &str) -> Option<String> {
    let pci = node
        .strip_prefix("/dev/dri/by-path/pci-")?
        .strip_suffix("-render")?;
    let b = pci.as_bytes();
    if b.len() != 12
        || b[4] != b':'
        || b[7] != b':'
        || b[10] != b'.'
        || !b
            .iter()
            .enumerate()
            .all(|(i, c)| matches!(i, 4 | 7 | 10) || c.is_ascii_hexdigit())
        || !(b'0'..=b'7').contains(&b[11])
    {
        return None;
    }
    Some(format!(
        "pci-{}",
        pci.replace([':', '.'], "_").to_ascii_lowercase()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_pkl_contract_fixtures() {
        let contract: serde_json::Value = serde_json::from_str(CONTRACT_JSON).unwrap();
        for case in contract["cases"].as_array().unwrap() {
            let node = case["node"].as_str().unwrap();
            assert_eq!(
                pci_selector(node).as_deref(),
                case["selector"].as_str(),
                "{node}"
            );
        }
    }
}
