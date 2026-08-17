use solana_pubkey::Pubkey;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedAccountState {
    pub parent: Pubkey,
    pub depth: u8,
    pub children_count: u64,
    pub recovery_authority: Option<Pubkey>,
    pub revoked: bool,
}




pub fn verify_lineage(
    chain: &[(Pubkey, DerivedAccountState)],
    expected_root: &Pubkey,
) -> bool {
    if chain.is_empty() {
        return false;
    }

    for i in 0..chain.len() {
        let (current_key, current_state) = &chain[i];

        if current_state.revoked {
            return false;
        }

        if i + 1 < chain.len() {
            let (next_key, next_state) = &chain[i + 1];
            if current_state.parent != *next_key {
                return false;
            }
            if current_state.depth != next_state.depth.saturating_add(1) {
                return false;
            }
        } else {
            if current_key != expected_root && current_state.parent != *expected_root {
                return false;
            }
        }
    }

    true
}



#[cfg(test)]
mod lineage_tests {
    use super::*;

    fn state(parent: Pubkey, depth: u8, revoked: bool) -> DerivedAccountState {
        DerivedAccountState {
            parent,
            depth,
            children_count: 0,
            recovery_authority: None,
            revoked,
        }
    }

    #[test]
    fn test_verify_lineage_valid_chain() {
        let root = Pubkey::new_unique();
        let pool = Pubkey::new_unique();
        let user = Pubkey::new_unique();

        let chain = vec![
            (user, state(pool, 2, false)),
            (pool, state(root, 1, false)),
        ];

        assert!(verify_lineage(&chain, &root));
    }

    #[test]
    fn test_verify_lineage_revoked_link_fails() {
        let root = Pubkey::new_unique();
        let pool = Pubkey::new_unique();
        let user = Pubkey::new_unique();

        let chain = vec![
            (user, state(pool, 2, false)),
            (pool, state(root, 1, true)), // revoked
        ];

        assert!(!verify_lineage(&chain, &root));
    }

    #[test]
    fn test_verify_lineage_wrong_parent_fails() {
        let root = Pubkey::new_unique();
        let pool = Pubkey::new_unique();
        let user = Pubkey::new_unique();
        let impostor = Pubkey::new_unique();

        let chain = vec![
            (user, state(impostor, 2, false)), // claims wrong parent
            (pool, state(root, 1, false)),
        ];

        assert!(!verify_lineage(&chain, &root));
    }

    #[test]
    fn test_verify_lineage_wrong_root_fails() {
        let root = Pubkey::new_unique();
        let fake_root = Pubkey::new_unique();
        let pool = Pubkey::new_unique();

        let chain = vec![(pool, state(root, 1, false))];

        assert!(!verify_lineage(&chain, &fake_root));
    }

    #[test]
    fn test_verify_lineage_empty_chain_fails() {
        let root = Pubkey::new_unique();
        assert!(!verify_lineage(&[], &root));
    }
}