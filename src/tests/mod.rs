#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use litesvm::{types::TransactionResult, LiteSVM};
    use litesvm_token::{spl_token, CreateAssociatedTokenAccount, CreateMint, MintTo};
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_message::Message;
    use solana_native_token::LAMPORTS_PER_SOL;
    use solana_program_pack::Pack;
    use solana_pubkey::Pubkey;
    use solana_signer::Signer;
    use solana_transaction::{InstructionError, Transaction, TransactionError};

    const INITIAL_A: u64 = 1_000_000_000;
    const GIVE_A: u64 = 500_000_000;
    const RECEIVE_B: u64 = 100_000_000;

    fn program_id() -> Pubkey {
        Pubkey::from(crate::ID)
    }

    fn setup() -> (LiteSVM, Keypair) {
        let mut svm = LiteSVM::new();
        let maker = Keypair::new();
        // Match pinocchio 0.11's rent calculation instead of LiteSVM's older default.
        #[allow(deprecated)]
        svm.set_sysvar(&solana_rent::Rent {
            lamports_per_byte_year: 6960,
            exemption_threshold: 1.0,
            burn_percent: 50,
        });
        svm.airdrop(&maker.pubkey(), 10 * LAMPORTS_PER_SOL).unwrap();
        let so_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target/deploy/escrow.so");
        let program_data = std::fs::read(&so_path).unwrap_or_else(|e| {
            panic!(
                "Failed to read {}: {e}. Run cargo build-sbf first.",
                so_path.display()
            )
        });
        svm.add_program(program_id(), &program_data).unwrap();
        (svm, maker)
    }

    struct MadeEscrow {
        svm: LiteSVM,
        maker: Keypair,
        mint_a: Pubkey,
        mint_b: Pubkey,
        escrow: Pubkey,
        bump: u8,
        vault: Pubkey,
        maker_ata_a: Pubkey,
        make_cus: u64,
    }

    fn send(
        svm: &mut LiteSVM,
        instruction: Instruction,
        fee_payer: &Keypair,
        signers: &[&Keypair],
    ) -> TransactionResult {
        let message = Message::new(&[instruction], Some(&fee_payer.pubkey()));
        let tx = Transaction::new(signers, message, svm.latest_blockhash());
        svm.send_transaction(tx)
    }

    fn make_escrow() -> MadeEscrow {
        let (mut svm, maker) = setup();
        let mint_a = CreateMint::new(&mut svm, &maker)
            .decimals(6)
            .authority(&maker.pubkey())
            .send()
            .unwrap();
        let mint_b = CreateMint::new(&mut svm, &maker)
            .decimals(6)
            .authority(&maker.pubkey())
            .send()
            .unwrap();
        let maker_ata_a = CreateAssociatedTokenAccount::new(&mut svm, &maker, &mint_a)
            .owner(&maker.pubkey())
            .send()
            .unwrap();
        MintTo::new(&mut svm, &maker, &mint_a, &maker_ata_a, INITIAL_A)
            .send()
            .unwrap();
        let (escrow, bump) =
            Pubkey::find_program_address(&[b"escrow", maker.pubkey().as_ref()], &program_id());
        let vault = ata(&escrow, &mint_a);
        let instruction = Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(maker.pubkey(), true),
                AccountMeta::new_readonly(mint_a, false),
                AccountMeta::new_readonly(mint_b, false),
                AccountMeta::new(escrow, false),
                AccountMeta::new(maker_ata_a, false),
                AccountMeta::new(vault, false),
                AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(spl_associated_token_account::ID, false),
            ],
            data: [
                vec![0],
                RECEIVE_B.to_le_bytes().to_vec(),
                GIVE_A.to_le_bytes().to_vec(),
            ]
            .concat(),
        };
        let result = send(&mut svm, instruction, &maker, &[&maker]).unwrap();
        println!(
            "Make compute_units_consumed: {}",
            result.compute_units_consumed
        );
        let fixture = MadeEscrow {
            svm,
            maker,
            mint_a,
            mint_b,
            escrow,
            bump,
            vault,
            maker_ata_a,
            make_cus: result.compute_units_consumed,
        };
        assert_deposit_intact(&fixture);
        fixture
    }

    fn ata(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
        spl_associated_token_account::get_associated_token_address(owner, mint)
    }

    fn token_amount(svm: &LiteSVM, address: &Pubkey) -> u64 {
        let account = svm.get_account(address).unwrap();
        assert_eq!(account.owner, spl_token::ID);
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    }

    fn assert_deposit_intact(f: &MadeEscrow) {
        assert_eq!(token_amount(&f.svm, &f.vault), GIVE_A);
        assert_eq!(token_amount(&f.svm, &f.maker_ata_a), INITIAL_A - GIVE_A);
        let vault = f.svm.get_account(&f.vault).unwrap();
        let vault_state = spl_token::state::Account::unpack(&vault.data).unwrap();
        assert_eq!(vault_state.owner, f.escrow);
        assert_eq!(vault_state.mint, f.mint_a);
        let escrow = f.svm.get_account(&f.escrow).unwrap();
        assert_eq!(escrow.owner, program_id());
        assert_eq!(escrow.data.len(), crate::state::Escrow::LEN);
        assert_eq!(&escrow.data[0..32], f.maker.pubkey().as_ref());
        assert_eq!(&escrow.data[32..64], f.mint_a.as_ref());
        assert_eq!(&escrow.data[64..96], f.mint_b.as_ref());
        assert_eq!(
            u64::from_le_bytes(escrow.data[96..104].try_into().unwrap()),
            RECEIVE_B
        );
        assert_eq!(
            u64::from_le_bytes(escrow.data[104..112].try_into().unwrap()),
            GIVE_A
        );
        assert_eq!(escrow.data[112], f.bump);
    }

    fn assert_closed(svm: &LiteSVM, address: &Pubkey) {
        if let Some(account) = svm.get_account(address) {
            assert_eq!(account.lamports, 0);
            assert_eq!(account.owner, solana_sdk_ids::system_program::ID);
            assert!(account.data.is_empty());
        }
    }

    fn rent_refund(f: &MadeEscrow) -> u64 {
        f.svm.get_account(&f.vault).unwrap().lamports
            + f.svm.get_account(&f.escrow).unwrap().lamports
    }

    fn prepare_taker(f: &mut MadeEscrow, amount_b: u64) -> Keypair {
        let taker = Keypair::new();
        f.svm
            .airdrop(&taker.pubkey(), 10 * LAMPORTS_PER_SOL)
            .unwrap();
        let taker_ata_b = CreateAssociatedTokenAccount::new(&mut f.svm, &taker, &f.mint_b)
            .owner(&taker.pubkey())
            .send()
            .unwrap();
        MintTo::new(&mut f.svm, &f.maker, &f.mint_b, &taker_ata_b, amount_b)
            .send()
            .unwrap();
        // Take must create both receiving ATAs itself.
        assert!(f
            .svm
            .get_account(&ata(&taker.pubkey(), &f.mint_a))
            .is_none());
        assert!(f
            .svm
            .get_account(&ata(&f.maker.pubkey(), &f.mint_b))
            .is_none());
        taker
    }

    fn take_instruction(f: &MadeEscrow, taker: &Keypair) -> Instruction {
        Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(taker.pubkey(), true),
                AccountMeta::new(f.maker.pubkey(), false),
                AccountMeta::new_readonly(f.mint_a, false),
                AccountMeta::new_readonly(f.mint_b, false),
                AccountMeta::new(f.escrow, false),
                AccountMeta::new(f.vault, false),
                AccountMeta::new(ata(&taker.pubkey(), &f.mint_a), false),
                AccountMeta::new(ata(&taker.pubkey(), &f.mint_b), false),
                AccountMeta::new(ata(&f.maker.pubkey(), &f.mint_b), false),
                AccountMeta::new_readonly(solana_sdk_ids::system_program::ID, false),
                AccountMeta::new_readonly(spl_token::ID, false),
                AccountMeta::new_readonly(spl_associated_token_account::ID, false),
            ],
            data: vec![1],
        }
    }

    fn cancel_instruction(f: &MadeEscrow) -> Instruction {
        Instruction {
            program_id: program_id(),
            accounts: vec![
                AccountMeta::new(f.maker.pubkey(), true),
                AccountMeta::new_readonly(f.mint_a, false),
                AccountMeta::new(f.escrow, false),
                AccountMeta::new(f.vault, false),
                AccountMeta::new(f.maker_ata_a, false),
                AccountMeta::new_readonly(spl_token::ID, false),
            ],
            data: vec![2],
        }
    }

    #[test]
    fn test_make_instruction() {
        let mut f = make_escrow();
        // Unsupported MakeV2 must explicitly fail, rather than silently succeeding.
        let instruction = Instruction {
            program_id: program_id(),
            accounts: vec![],
            data: vec![3],
        };
        let failure = send(&mut f.svm, instruction, &f.maker, &[&f.maker]).unwrap_err();
        assert_eq!(
            failure.err,
            TransactionError::InstructionError(0, InstructionError::InvalidInstructionData)
        );
        assert_deposit_intact(&f);
    }

    #[test]
    fn test_take_instruction() {
        let mut f = make_escrow();
        let taker = prepare_taker(&mut f, RECEIVE_B);
        let instruction = take_instruction(&f, &taker);
        let maker_balance = f.svm.get_account(&f.maker.pubkey()).unwrap().lamports;
        let refund = rent_refund(&f);
        let result = send(&mut f.svm, instruction, &taker, &[&taker]).unwrap();
        println!(
            "Take compute_units_consumed: {} (Make: {})",
            result.compute_units_consumed, f.make_cus
        );
        assert_eq!(
            token_amount(&f.svm, &ata(&taker.pubkey(), &f.mint_a)),
            GIVE_A
        );
        assert_eq!(
            token_amount(&f.svm, &ata(&f.maker.pubkey(), &f.mint_b)),
            RECEIVE_B
        );
        assert_eq!(token_amount(&f.svm, &ata(&taker.pubkey(), &f.mint_b)), 0);
        assert_eq!(token_amount(&f.svm, &f.maker_ata_a), INITIAL_A - GIVE_A);
        assert_closed(&f.svm, &f.vault);
        assert_closed(&f.svm, &f.escrow);
        assert_eq!(
            f.svm.get_account(&f.maker.pubkey()).unwrap().lamports,
            maker_balance + refund
        );
    }

    #[test]
    fn test_cancel_instruction() {
        let mut f = make_escrow();
        let instruction = cancel_instruction(&f);
        let maker_balance = f.svm.get_account(&f.maker.pubkey()).unwrap().lamports;
        let refund = rent_refund(&f);
        let result = send(&mut f.svm, instruction, &f.maker, &[&f.maker]).unwrap();
        println!(
            "Cancel compute_units_consumed: {} (Make: {})",
            result.compute_units_consumed, f.make_cus
        );
        assert_eq!(token_amount(&f.svm, &f.maker_ata_a), INITIAL_A);
        assert_closed(&f.svm, &f.vault);
        assert_closed(&f.svm, &f.escrow);
        assert_eq!(
            f.svm.get_account(&f.maker.pubkey()).unwrap().lamports,
            maker_balance + refund - result.fee
        );
    }

    #[test]
    fn test_take_insufficient_funds() {
        let mut f = make_escrow();
        let taker = prepare_taker(&mut f, RECEIVE_B / 2);
        let instruction = take_instruction(&f, &taker);
        let maker_balance = f.svm.get_account(&f.maker.pubkey()).unwrap().lamports;
        let escrow_before = f.svm.get_account(&f.escrow).unwrap();
        let vault_before = f.svm.get_account(&f.vault).unwrap();
        let failure = send(&mut f.svm, instruction, &taker, &[&taker]).unwrap_err();
        println!(
            "Underfunded Take compute_units_consumed: {}",
            failure.meta.compute_units_consumed
        );
        assert_eq!(
            failure.err,
            TransactionError::InstructionError(
                0,
                InstructionError::Custom(spl_token::error::TokenError::InsufficientFunds as u32)
            )
        );
        assert_deposit_intact(&f);
        assert_eq!(f.svm.get_account(&f.escrow).unwrap(), escrow_before);
        assert_eq!(f.svm.get_account(&f.vault).unwrap(), vault_before);
        assert_eq!(
            token_amount(&f.svm, &ata(&taker.pubkey(), &f.mint_b)),
            RECEIVE_B / 2
        );
        // The earlier CreateIdempotent CPIs must roll back with the failed payment.
        assert!(f
            .svm
            .get_account(&ata(&taker.pubkey(), &f.mint_a))
            .is_none());
        assert!(f
            .svm
            .get_account(&ata(&f.maker.pubkey(), &f.mint_b))
            .is_none());
        assert_eq!(
            f.svm.get_account(&f.maker.pubkey()).unwrap().lamports,
            maker_balance
        );
    }

    #[test]
    fn test_cancel_by_stranger() {
        let mut f = make_escrow();
        let stranger = Keypair::new();
        f.svm.airdrop(&stranger.pubkey(), LAMPORTS_PER_SOL).unwrap();
        let stranger_ata = CreateAssociatedTokenAccount::new(&mut f.svm, &stranger, &f.mint_a)
            .owner(&stranger.pubkey())
            .send()
            .unwrap();
        let escrow_before = f.svm.get_account(&f.escrow).unwrap();
        let vault_before = f.svm.get_account(&f.vault).unwrap();
        let maker_balance = f.svm.get_account(&f.maker.pubkey()).unwrap().lamports;

        // Correct maker address but no maker signature: isolate the signer check.
        let mut unsigned = cancel_instruction(&f);
        unsigned.accounts[0].is_signer = false;
        let failure = send(&mut f.svm, unsigned, &stranger, &[&stranger]).unwrap_err();
        println!(
            "Unsigned Cancel compute_units_consumed: {}",
            failure.meta.compute_units_consumed
        );
        assert_eq!(
            failure.err,
            TransactionError::InstructionError(0, InstructionError::MissingRequiredSignature)
        );
        assert_deposit_intact(&f);

        // A valid signature from the wrong maker: isolate the stored-maker check.
        let mut wrong_maker = cancel_instruction(&f);
        wrong_maker.accounts[0] = AccountMeta::new(stranger.pubkey(), true);
        wrong_maker.accounts[4] = AccountMeta::new(stranger_ata, false);
        let failure = send(&mut f.svm, wrong_maker, &stranger, &[&stranger]).unwrap_err();
        println!(
            "Stranger Cancel compute_units_consumed: {}",
            failure.meta.compute_units_consumed
        );
        assert_eq!(
            failure.err,
            TransactionError::InstructionError(0, InstructionError::InvalidAccountData)
        );
        assert_deposit_intact(&f);
        assert_eq!(token_amount(&f.svm, &stranger_ata), 0);
        assert_eq!(f.svm.get_account(&f.escrow).unwrap(), escrow_before);
        assert_eq!(f.svm.get_account(&f.vault).unwrap(), vault_before);
        assert_eq!(
            f.svm.get_account(&f.maker.pubkey()).unwrap().lamports,
            maker_balance
        );

        // Retain the destination and mint checks covered before this refactor.
        let mut wrong_destination = cancel_instruction(&f);
        wrong_destination.accounts[4] = AccountMeta::new(stranger_ata, false);
        let failure = send(
            &mut f.svm,
            wrong_destination,
            &stranger,
            &[&stranger, &f.maker],
        )
        .unwrap_err();
        assert_eq!(
            failure.err,
            TransactionError::InstructionError(0, InstructionError::IllegalOwner)
        );
        let mut wrong_mint = cancel_instruction(&f);
        wrong_mint.accounts[1] = AccountMeta::new_readonly(f.mint_b, false);
        let failure = send(&mut f.svm, wrong_mint, &stranger, &[&stranger, &f.maker]).unwrap_err();
        assert_eq!(
            failure.err,
            TransactionError::InstructionError(0, InstructionError::InvalidAccountData)
        );
        assert_deposit_intact(&f);
    }
}
