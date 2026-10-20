//! Prints the regtest miner address that `infra/zakura/regtest.toml` pays coinbase to.

fn main() {
    let miner = zchan_tx::keys::p2pkh(&zchan_tx::keys::pubkey(&zchan_tx::keys::miner()));
    println!("miner {}", zchan_tx::keys::encode_regtest(&miner));
}
