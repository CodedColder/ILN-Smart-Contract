/// Invoice NFT Module
///
/// Implements Stellar NFT standard for invoice representation on Soroban.
/// Each invoice is represented as a unique NFT that:
/// - Is minted when invoice is first funded (PartiallyFunded / Funded)
/// - Transferred when LP position changes or lead LP changes
/// - Burned when invoice is marked as paid or cancelled/refunded
///
/// INVARIANT:
/// An NFT representing an invoice exists if and only if the invoice status is
/// Funded, PartiallyFunded, Defaulted, Appealed, or Disputed.
/// The NFT owner (holder) always equals the current LP (funder) for fully funded
/// invoices, or the lead LP (the LP with the largest contribution) for partially funded
/// invoices. For other statuses (Pending, Paid, Expired, Cancelled), the NFT does not exist.
///
/// NFT Metadata contains:
/// - Invoice ID
/// - Amount
/// - Due date
/// - Discount rate
/// - Token address
use soroban_sdk::{contracttype, Address, Env, Symbol};

use crate::errors::ContractError;
use crate::events::{InvoiceNftBurned, InvoiceNftMinted, InvoiceNftTransferred};
use crate::storage::DataKey;

/// NFT Metadata: complete information about an invoice NFT
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct InvoiceNftMetadata {
    /// The invoice ID this NFT represents
    pub invoice_id: u64,
    /// Full invoice amount in stroops
    pub amount: i128,
    /// Unix timestamp of when the invoice is due
    pub due_date: u32,
    /// Discount rate in basis points (e.g. 300 = 3.00%)
    pub discount_rate: u32,
    /// Token used for the invoice
    pub token: Address,
    /// Current owner of the NFT
    pub owner: Address,
    /// Timestamp when the NFT was minted
    pub minted_at: u32,
}

/// Get the storage key for an invoice NFT by its invoice ID
fn get_nft_key(invoice_id: u64) -> DataKey {
    DataKey::InvoiceNft(invoice_id)
}

/// Get the storage key for NFT ownership tracking (for queries)
fn get_nft_owner_key(invoice_id: u64) -> DataKey {
    DataKey::InvoiceNftOwner(invoice_id)
}

/// Mint an NFT representing an invoice
///
/// # Arguments
/// * `env` - Soroban environment
/// * `invoice_id` - Unique invoice identifier
/// * `owner` - Initial owner of the NFT (the freelancer/submitter)
/// * `amount` - Invoice amount
/// * `due_date` - Invoice due date
/// * `discount_rate` - Discount rate in basis points
/// * `token` - Token address used for the invoice
///
/// # Returns
/// Result with unit on success or ContractError on failure
pub fn mint_invoice_nft(
    env: &Env,
    invoice_id: u64,
    owner: Address,
    amount: i128,
    due_date: u32,
    discount_rate: u32,
    token: Address,
) -> Result<(), ContractError> {
    // Check that NFT doesn't already exist
    if env.storage().persistent().has(&get_nft_key(invoice_id)) {
        return Err(ContractError::AlreadyFunded);
    }

    let metadata = InvoiceNftMetadata {
        invoice_id,
        amount,
        due_date,
        discount_rate,
        token,
        owner: owner.clone(),
        minted_at: env.ledger().timestamp() as u32,
    };

    env.storage()
        .persistent()
        .set(&get_nft_key(invoice_id), &metadata);

    env.storage()
        .persistent()
        .set(&get_nft_owner_key(invoice_id), &owner);

    // Publish NFT minting event (Soroban SDK: publish(topics, data))
    env.events().publish(
        (
            Symbol::new(env, "invoice_nft_minted"),
            invoice_id,
            owner.clone(),
        ),
        InvoiceNftMinted {
            invoice_id,
            owner,
            amount,
            due_date,
            timestamp: env.ledger().timestamp(),
        },
    );

    Ok(())
}

/// Transfer an invoice NFT from one owner to another
///
/// # Arguments
/// * `env` - Soroban environment
/// * `invoice_id` - Invoice ID of the NFT to transfer
/// * `from` - Current owner
/// * `to` - New owner
///
/// # Returns
/// Result with unit on success or ContractError on failure
pub fn transfer_invoice_nft(
    env: &Env,
    invoice_id: u64,
    from: Address,
    to: Address,
) -> Result<(), ContractError> {
    // Load metadata
    let mut metadata: InvoiceNftMetadata = env
        .storage()
        .persistent()
        .get(&get_nft_key(invoice_id))
        .ok_or(ContractError::InvoiceNotFound)?;

    // Verify current owner
    if metadata.owner != from {
        return Err(ContractError::Unauthorized);
    }

    // Update owner
    metadata.owner = to.clone();

    env.storage()
        .persistent()
        .set(&get_nft_key(invoice_id), &metadata);

    env.storage()
        .persistent()
        .set(&get_nft_owner_key(invoice_id), &to);

    // Publish NFT transfer event (Soroban SDK: publish(topics, data))
    env.events().publish(
        (
            Symbol::new(env, "invoice_nft_transferred"),
            invoice_id,
            from.clone(),
            to.clone(),
        ),
        InvoiceNftTransferred {
            invoice_id,
            from,
            to,
            timestamp: env.ledger().timestamp(),
        },
    );

    Ok(())
}

/// Burn (destroy) an invoice NFT
///
/// # Arguments
/// * `env` - Soroban environment
/// * `invoice_id` - Invoice ID of the NFT to burn
/// * `owner` - Current owner (for authorization)
///
/// # Returns
/// Result with unit on success or ContractError on failure
pub fn burn_invoice_nft(env: &Env, invoice_id: u64, owner: Address) -> Result<(), ContractError> {
    // Load metadata for event emission
    let metadata: InvoiceNftMetadata = env
        .storage()
        .persistent()
        .get(&get_nft_key(invoice_id))
        .ok_or(ContractError::InvoiceNotFound)?;

    // Verify current owner
    if metadata.owner != owner {
        return Err(ContractError::Unauthorized);
    }

    // Remove NFT metadata
    env.storage().persistent().remove(&get_nft_key(invoice_id));

    // Remove owner tracking
    env.storage()
        .persistent()
        .remove(&get_nft_owner_key(invoice_id));

    // Publish NFT burn event (Soroban SDK: publish(topics, data))
    env.events().publish(
        (
            Symbol::new(env, "invoice_nft_burned"),
            invoice_id,
            owner.clone(),
        ),
        InvoiceNftBurned {
            invoice_id,
            owner,
            timestamp: env.ledger().timestamp(),
        },
    );

    Ok(())
}

/// Get the metadata of an invoice NFT
///
/// # Arguments
/// * `env` - Soroban environment
/// * `invoice_id` - Invoice ID
///
/// # Returns
/// Option containing the metadata if it exists
pub fn get_invoice_nft_metadata(env: &Env, invoice_id: u64) -> Option<InvoiceNftMetadata> {
    env.storage().persistent().get(&get_nft_key(invoice_id))
}

/// Get the current owner of an invoice NFT
///
/// # Arguments
/// * `env` - Soroban environment
/// * `invoice_id` - Invoice ID
///
/// # Returns
/// Option containing the owner address if the NFT exists
pub fn get_invoice_nft_owner(env: &Env, invoice_id: u64) -> Option<Address> {
    env.storage()
        .persistent()
        .get(&get_nft_owner_key(invoice_id))
}

/// Check if an invoice NFT exists
///
/// # Arguments
/// * `env` - Soroban environment
/// * `invoice_id` - Invoice ID
///
/// # Returns
/// true if the NFT exists, false otherwise
pub fn invoice_nft_exists(env: &Env, invoice_id: u64) -> bool {
    env.storage().persistent().has(&get_nft_key(invoice_id))
}

/// Get invoice NFT metadata (publicly callable query function)
pub fn query_nft_metadata(env: Env, invoice_id: u64) -> Option<InvoiceNftMetadata> {
    get_invoice_nft_metadata(&env, invoice_id)
}

/// Get NFT owner (publicly callable query function)
pub fn query_nft_owner(env: Env, invoice_id: u64) -> Option<Address> {
    get_invoice_nft_owner(&env, invoice_id)
}

/// Sync NFT state with the corresponding invoice.
/// Automatically mints, transfers, or burns the NFT to maintain the invariant:
/// NFT exists iff invoice is Funded/PartiallyFunded/settled-pending-burn (Defaulted/Appealed/Disputed),
/// and the holder is the LP (funder) or the lead LP for partial funding.
pub fn sync_nft_state(env: &Env, invoice_id: u64) -> Result<(), ContractError> {
    use crate::invoice::{get_invoice_funders, try_load_invoice, InvoiceStatus};

    let invoice = match try_load_invoice(env, invoice_id) {
        Some(inv) => inv,
        None => {
            // If the invoice doesn't exist at all, we should ensure the NFT doesn't exist
            if invoice_nft_exists(env, invoice_id) {
                let current_owner = get_invoice_nft_owner(env, invoice_id).unwrap();
                burn_invoice_nft(env, invoice_id, current_owner)?;
            }
            return Ok(());
        }
    };

    let should_exist = match invoice.status {
        InvoiceStatus::Funded
        | InvoiceStatus::PartiallyFunded
        | InvoiceStatus::Defaulted
        | InvoiceStatus::Appealed
        | InvoiceStatus::Disputed => true,
        _ => false,
    };

    if should_exist {
        // Determine the current owner/holder of the NFT.
        // For Funded/Defaulted/Appealed/Disputed with single funder: invoice.funder is Some.
        // For PartiallyFunded (or if invoice.funder is None): we look up from the funders list.
        let target_owner = if let Some(ref lp) = invoice.funder {
            lp.clone()
        } else {
            let funders = get_invoice_funders(env, invoice_id);
            if funders.is_empty() {
                // If it is partially funded but list is empty (should not happen), fallback to freelancer
                invoice.freelancer.clone()
            } else {
                // Find the funder with the maximum funded amount (lead LP).
                // Ties broken by first-in-list (earliest funder).
                let mut lead_lp = funders.get(0).unwrap().0;
                let mut max_amt = funders.get(0).unwrap().1;
                for i in 1..funders.len() {
                    let (addr, amt) = funders.get(i).unwrap();
                    if amt > max_amt {
                        max_amt = amt;
                        lead_lp = addr;
                    }
                }
                lead_lp
            }
        };

        if invoice_nft_exists(env, invoice_id) {
            let current_owner =
                get_invoice_nft_owner(env, invoice_id).ok_or(ContractError::InvoiceNotFound)?;
            if current_owner != target_owner {
                transfer_invoice_nft(env, invoice_id, current_owner, target_owner)?;
            }
        } else {
            mint_invoice_nft(
                env,
                invoice_id,
                target_owner,
                invoice.amount,
                invoice.due_date as u32,
                invoice.discount_rate,
                invoice.token.clone(),
            )?;
        }
    } else {
        // Should not exist
        if invoice_nft_exists(env, invoice_id) {
            let current_owner =
                get_invoice_nft_owner(env, invoice_id).ok_or(ContractError::InvoiceNotFound)?;
            burn_invoice_nft(env, invoice_id, current_owner)?;
        }
    }

    Ok(())
}
