#[cfg(test)]
mod tests {

    use std::path::PathBuf;

    use litesvm::LiteSVM;
    use litesvm_token::{spl_token::{self}, CreateAssociatedTokenAccount, CreateMint, MintTo};
    
    use solana_instruction::{AccountMeta, Instruction};
    use solana_keypair::Keypair;
    use solana_message::Message;
    use solana_native_token::LAMPORTS_PER_SOL;
    use solana_pubkey::Pubkey;
    use solana_signer::Signer;
    use solana_transaction::Transaction;
    use solana_program_pack::Pack;

    const PROGRAM_ID: &str = "4ibrEMW5F6hKnkW4jVedswYv6H6VtwPN6ar6dvXDN1nT";
    const TOKEN_PROGRAM_ID: Pubkey = spl_token::ID;
    const ASSOCIATED_TOKEN_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
    
    fn program_id() -> Pubkey {
        Pubkey::from(crate::ID)
    }

    fn setup() -> (LiteSVM, Keypair) {

        let mut svm = LiteSVM::new();
        let payer = Keypair::new();

        // LiteSVM 0.9 still ships the pre-SIMD-0194 Rent sysvar (3480 lamports/byte-year,
        // 2-year exemption threshold). Mainnet has activated SIMD-0194, which folds the
        // threshold into the rate (6960 lamports/byte, threshold 1.0), and pinocchio 0.11
        // computes rent exemption that way. Set the sysvar to match the live cluster.
        #[allow(deprecated)]
        svm.set_sysvar(&solana_rent::Rent {
            lamports_per_byte_year: 6960,
            exemption_threshold: 1.0,
            burn_percent: 50,
        });

        svm
            .airdrop(&payer.pubkey(), 10 * LAMPORTS_PER_SOL)
            .expect("Airdrop failed");

        // Load program SO file (produced by `cargo build-sbf`)
        let so_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target/deploy/escrow.so");

        let program_data = std::fs::read(&so_path)
            .unwrap_or_else(|e| panic!("Failed to read program SO file at {}: {e}. Run `cargo build-sbf` first.", so_path.display()));
    
        svm.add_program(program_id(), &program_data).expect("Failed to add program");

        (svm, payer)
        
    }

    #[test]
    pub fn test_make_and_cancel_instruction() {
        let (mut svm, payer) = setup();

        let program_id = program_id();

        assert_eq!(program_id.to_string(), PROGRAM_ID);

        let mint_a = CreateMint::new(&mut svm, &payer)
            .decimals(6)
            .authority(&payer.pubkey())
            .send()
            .unwrap();
        println!("Mint A: {}", mint_a);

        let mint_b = CreateMint::new(&mut svm, &payer)
            .decimals(6)
            .authority(&payer.pubkey())
            .send()
            .unwrap();
        println!("Mint B: {}", mint_b);

        // Create the maker's associated token account for Mint A
        let maker_ata_a = CreateAssociatedTokenAccount::new(&mut svm, &payer, &mint_a)
            .owner(&payer.pubkey()).send().unwrap();
        println!("Maker ATA A: {}\n", maker_ata_a);

        // Derive the PDA for the escrow account using the maker's public key and a seed value
        let escrow = Pubkey::find_program_address(
            &[b"escrow".as_ref(), payer.pubkey().as_ref()],
            &PROGRAM_ID.parse().unwrap(),
        );
        println!("Escrow PDA: {}\n", escrow.0);

        // Derive the PDA for the vault associated token account using the escrow PDA and Mint A
        let vault = spl_associated_token_account::get_associated_token_address(
            &escrow.0,  // owner will be the escrow PDA
            &mint_a     // mint
        );
        println!("Vault PDA: {}\n", vault);

        // Define program IDs for associated token program, token program, and system program
        let associated_token_program = ASSOCIATED_TOKEN_PROGRAM_ID.parse::<Pubkey>().unwrap();
        let token_program = TOKEN_PROGRAM_ID;
        let system_program = solana_sdk_ids::system_program::ID;

        // Mint 1,000 tokens (with 6 decimal places) of Mint A to the maker's associated token account
        MintTo::new(&mut svm, &payer, &mint_a, &maker_ata_a, 1000000000)
            .send()
            .unwrap();

        let amount_to_receive: u64 = 100000000; // 100 tokens with 6 decimal places
        let amount_to_give: u64 = 500000000;    // 500 tokens with 6 decimal places
        let bump: u8 = escrow.1;   // canonical bump; the program derives the same one on-chain

        println!("Bump: {}", bump);

        // Create the "Make" instruction to deposit tokens into the escrow
        let make_data = [
            vec![0u8],              // Discriminator for "Make" instruction
            amount_to_receive.to_le_bytes().to_vec(),
            amount_to_give.to_le_bytes().to_vec(),
        ].concat();
        let make_ix = Instruction {
            program_id: program_id,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new(mint_a, false),
                AccountMeta::new(mint_b, false),
                AccountMeta::new(escrow.0, false),
                AccountMeta::new(maker_ata_a, false),
                AccountMeta::new(vault, false),
                AccountMeta::new(system_program, false),
                AccountMeta::new(token_program, false),
                AccountMeta::new(associated_token_program, false),
            ],
            data: make_data,
        };

        // Create and send the transaction containing the "Make" instruction
        let message = Message::new(&[make_ix], Some(&payer.pubkey()));
        let recent_blockhash = svm.latest_blockhash();

        let transaction = Transaction::new(&[&payer], message, recent_blockhash);

        // Send the transaction and capture the result
        let tx = svm.send_transaction(transaction).unwrap();

        // Log transaction details
        println!("\n\nMake transaction successful");
        println!("CUs Consumed: {}", tx.compute_units_consumed);

        // --- extra verification ---
        let vault_acc = svm.get_account(&vault).unwrap();
        let vault_state = spl_token_2022::state::Account::unpack(&vault_acc.data).unwrap();
        println!("Vault owner: {} (escrow PDA? {})", vault_state.owner, vault_state.owner == escrow.0);
        println!("Vault balance: {}", vault_state.amount);
        assert_eq!(vault_state.amount, amount_to_give);

        let maker_acc = svm.get_account(&maker_ata_a).unwrap();
        let maker_state = spl_token_2022::state::Account::unpack(&maker_acc.data).unwrap();
        println!("Maker ATA balance: {}", maker_state.amount);
        assert_eq!(maker_state.amount, 1000000000 - amount_to_give);

        let esc = svm.get_account(&escrow.0).unwrap();
        println!("Escrow account owner: {} (program? {})", esc.owner, esc.owner == program_id);
        println!("Escrow data len: {}", esc.data.len());
        let d = &esc.data;
        println!("  maker   = {}", Pubkey::new_from_array(d[0..32].try_into().unwrap()));
        println!("  mint_a  = {}", Pubkey::new_from_array(d[32..64].try_into().unwrap()));
        println!("  mint_b  = {}", Pubkey::new_from_array(d[64..96].try_into().unwrap()));
        println!("  receive = {}", u64::from_le_bytes(d[96..104].try_into().unwrap()));
        println!("  give    = {}", u64::from_le_bytes(d[104..112].try_into().unwrap()));
        println!("  bump    = {}", d[112]);
        assert_eq!(&d[0..32], payer.pubkey().as_ref());
        assert_eq!(u64::from_le_bytes(d[96..104].try_into().unwrap()), amount_to_receive);
        assert_eq!(u64::from_le_bytes(d[104..112].try_into().unwrap()), amount_to_give);
        assert_eq!(d[112], bump);

        let cancel_ix = Instruction {
            program_id,
            accounts: vec![
                AccountMeta::new(payer.pubkey(), true),
                AccountMeta::new_readonly(mint_a, false),
                AccountMeta::new(escrow.0, false),
                AccountMeta::new(vault, false),
                AccountMeta::new(maker_ata_a, false),
                AccountMeta::new_readonly(token_program, false),
            ],
            data: vec![2],
        };

        // A separate fee payer prevents the maker becoming a signer implicitly.
        let attacker = Keypair::new();
        svm.airdrop(&attacker.pubkey(), LAMPORTS_PER_SOL).unwrap();
        let attacker_ata = CreateAssociatedTokenAccount::new(&mut svm, &attacker, &mint_a)
            .owner(&attacker.pubkey()).send().unwrap();

        let mut unsigned = cancel_ix.clone();
        unsigned.accounts[0].is_signer = false;
        let message = Message::new(&[unsigned], Some(&attacker.pubkey()));
        let tx = Transaction::new(&[&attacker], message, svm.latest_blockhash());
        let error = svm.send_transaction(tx).unwrap_err();
        assert!(format!("{:?}", error.err).contains("MissingRequiredSignature"));

        let mut wrong_maker = cancel_ix.clone();
        wrong_maker.accounts[0] = AccountMeta::new(attacker.pubkey(), true);
        let message = Message::new(&[wrong_maker], Some(&attacker.pubkey()));
        let tx = Transaction::new(&[&attacker], message, svm.latest_blockhash());
        let error = svm.send_transaction(tx).unwrap_err();
        assert!(format!("{:?}", error.err).contains("InvalidAccountData"));

        let mut wrong_mint = cancel_ix.clone();
        wrong_mint.accounts[1] = AccountMeta::new_readonly(mint_b, false);
        let message = Message::new(&[wrong_mint], Some(&attacker.pubkey()));
        let tx = Transaction::new(&[&attacker, &payer], message, svm.latest_blockhash());
        let error = svm.send_transaction(tx).unwrap_err();
        assert!(format!("{:?}", error.err).contains("InvalidAccountData"));

        let mut wrong_destination = cancel_ix.clone();
        wrong_destination.accounts[4] = AccountMeta::new(attacker_ata, false);
        let message = Message::new(&[wrong_destination], Some(&attacker.pubkey()));
        let tx = Transaction::new(&[&attacker, &payer], message, svm.latest_blockhash());
        let error = svm.send_transaction(tx).unwrap_err();
        assert!(format!("{:?}", error.err).contains("IllegalOwner"));

        // Failed attempts must leave the deposit intact.
        let vault_account = svm.get_account(&vault).unwrap();
        assert_eq!(spl_token_2022::state::Account::unpack(&vault_account.data).unwrap().amount, amount_to_give);
        let rent_refund = vault_account.lamports + svm.get_account(&escrow.0).unwrap().lamports;
        let maker_balance = svm.get_account(&payer.pubkey()).unwrap().lamports;

        // The attacker pays the fee so the maker's exact rent refund is observable.
        let message = Message::new(&[cancel_ix], Some(&attacker.pubkey()));
        let tx = Transaction::new(&[&attacker, &payer], message, svm.latest_blockhash());
        svm.send_transaction(tx).unwrap();

        let maker_tokens = svm.get_account(&maker_ata_a).unwrap();
        assert_eq!(spl_token_2022::state::Account::unpack(&maker_tokens.data).unwrap().amount, 1000000000);
        assert_eq!(svm.get_account(&payer.pubkey()).unwrap().lamports, maker_balance + rent_refund);
        for address in [vault, escrow.0] {
            if let Some(account) = svm.get_account(&address) {
                assert_eq!(account.lamports, 0);
                assert!(account.data.is_empty());
            }
        }

        let unsupported = Instruction { program_id, accounts: vec![], data: vec![3] };
        let message = Message::new(&[unsupported], Some(&attacker.pubkey()));
        let tx = Transaction::new(&[&attacker], message, svm.latest_blockhash());
        let error = svm.send_transaction(tx).unwrap_err();
        assert!(format!("{:?}", error.err).contains("InvalidInstructionData"));
    }
}
