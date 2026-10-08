use pinocchio::{
    cpi::{Seed, Signer}, error::ProgramError, AccountView, ProgramResult,
};
use pinocchio_pubkey::derive_address;
use pinocchio_token::{instructions::{CloseAccount, Transfer}, state::Account};

use crate::state::Escrow;

pub fn process_cancel_instruction(accounts: &mut [AccountView]) -> ProgramResult {
    let [maker, mint_a, escrow_account, vault, maker_ata_a, token_program, ..] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };

    if !maker.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }
    if !escrow_account.owned_by(&crate::ID) {
        return Err(ProgramError::IncorrectProgramId);
    }
    if token_program.address() != &pinocchio_token::ID {
        return Err(ProgramError::IncorrectProgramId);
    }

    let bump = {
        let escrow = Escrow::load_mut(escrow_account)?;
        if escrow.maker() != *maker.address() || escrow.mint_a() != *mint_a.address() {
            return Err(ProgramError::InvalidAccountData);
        }
        escrow.bump
    };

    let escrow_pda = derive_address(
        &[b"escrow", maker.address().as_ref()], Some(bump), &crate::ID.to_bytes(),
    );
    if escrow_pda != escrow_account.address().to_bytes() {
        return Err(ProgramError::InvalidAccountData);
    }

    let vault_amount = {
        let vault_state = Account::from_account_view(vault)?;
        if vault_state.owner() != escrow_account.address() {
            return Err(ProgramError::IllegalOwner);
        }
        if vault_state.mint() != mint_a.address() {
            return Err(ProgramError::InvalidAccountData);
        }
        vault_state.amount()
    };
    {
        let destination = Account::from_account_view(maker_ata_a)?;
        if destination.owner() != maker.address() {
            return Err(ProgramError::IllegalOwner);
        }
        if destination.mint() != mint_a.address() {
            return Err(ProgramError::InvalidAccountData);
        }
    }

    // Own the seed bytes so no account references remain when closing the escrow.
    let maker_address = maker.address().to_bytes();
    let bump_bytes = [bump];
    let seeds = [Seed::from(b"escrow"), Seed::from(&maker_address), Seed::from(&bump_bytes)];
    let signer = Signer::from(&seeds);

    Transfer {
        from: vault,
        to: maker_ata_a,
        authority: escrow_account,
        multisig_signers: &[] as &[&AccountView],
        amount: vault_amount,
    }.invoke_signed(&[signer.clone()])?;

    CloseAccount {
        account: vault,
        destination: maker,
        authority: escrow_account,
        multisig_signers: &[] as &[&AccountView],
    }.invoke_signed(&[signer])?;

    let maker_lamports = maker.lamports()
        .checked_add(escrow_account.lamports())
        .ok_or(ProgramError::ArithmeticOverflow)?;
    maker.set_lamports(maker_lamports);
    escrow_account.set_lamports(0);
    escrow_account.close()?;

    Ok(())
}
