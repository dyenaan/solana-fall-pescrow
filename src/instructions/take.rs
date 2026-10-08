use pinocchio::{cpi::{Seed, Signer}, error::ProgramError, AccountView, ProgramResult};

use pinocchio_pubkey::derive_address;
use pinocchio_token::instructions::CloseAccount;


use crate::state::Escrow;




pub fn process_take_instruction(
    accounts: &mut [AccountView],
)-> ProgramResult{
    let[
        taker,
        maker,
        mint_a,
        mint_b,
        escrow_account,
        vault,
        taker_ata_a,
        taker_ata_b,
        maker_ata_b,
        system_program,
        token_program,
        _associated_token_program @ ..
    ] = accounts else {
        return Err(ProgramError::NotEnoughAccountKeys);
    };
    //verify the taker account is a signer
    if !taker.is_signer() {
        return Err(ProgramError::MissingRequiredSignature);
    }

    if !escrow_account.owned_by(&crate::ID) {
        return Err(ProgramError::IncorrectProgramId);
    }


    let(amount_to_receive, bump) = {
        let escrow = Escrow::load_mut(escrow_account)?;
        if escrow.maker() != *maker.address() { return Err(ProgramError::InvalidAccountData); }
        if escrow.mint_a() != *mint_a.address() { return Err(ProgramError::InvalidAccountData); }
        if escrow.mint_b() != *mint_b.address() { return Err(ProgramError::InvalidAccountData); }
        (escrow.amount_to_receive(), escrow.bump)
    };


    let escrow_account_pda = derive_address(&[b"escrow", maker.address().as_ref()], Some(bump), &crate::ID.to_bytes());

    if escrow_account_pda != escrow_account.address().to_bytes() {
        return Err(ProgramError::InvalidAccountData);
    }

    //scope the borrow
   
    let taker_amount = {
        let vault_ata_state = pinocchio_token::state::Account::from_account_view(vault)?;
        if vault_ata_state.owner() != escrow_account.address(){
            return Err(ProgramError::IllegalOwner);
        } 
        if vault_ata_state.mint() != mint_a.address() {
            return Err(ProgramError::InvalidAccountData);
        }
        vault_ata_state.amount()
    };

    pinocchio_associated_token_account::instructions::CreateIdempotent{
        funding_account: taker,
        account: &taker_ata_a,
        wallet: taker,
        mint: mint_a,
        token_program,
        system_program
    }.invoke()?;

    pinocchio_associated_token_account::instructions::CreateIdempotent {
        funding_account: taker,
        account: maker_ata_b,
        wallet: maker,
        mint: mint_b,
        token_program,
        system_program,
    }.invoke()?;

    pinocchio_token::instructions::Transfer {
        from:taker_ata_b,
        to:maker_ata_b,
        authority: taker,
        multisig_signers: &[] as &[&AccountView],
        amount : amount_to_receive,
    }.invoke()?;

   //generate signer for this transfer since its escrow pda invoked
   let bump_bytes = [bump];
   let seed = [Seed::from(b"escrow"), Seed::from(maker.address().as_array()), Seed::from(&bump_bytes)];
   let signer = Signer::from(&seed);


    pinocchio_token::instructions::Transfer {
        from: vault,
        to: taker_ata_a,
        authority: escrow_account,
        multisig_signers: &[] as &[&AccountView],
        amount: taker_amount,
    }.invoke_signed(&[signer.clone()])?;


    CloseAccount{
        account: vault,
        destination: maker,
        authority: escrow_account,
        multisig_signers: &[] as &[&AccountView],
    }.invoke_signed(&[signer.clone()])?;

    let maker_lamports = maker.lamports()
                        .checked_add(escrow_account.lamports())
                        .ok_or(ProgramError::ArithmeticOverflow)?;
    maker.set_lamports(maker_lamports);
    escrow_account.set_lamports(0);

    escrow_account.close()?;

    Ok(())

}