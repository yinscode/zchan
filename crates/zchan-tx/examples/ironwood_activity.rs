//! Prints how many Ironwood commitments a node's tree gained between two heights.
//! `cargo run --example ironwood_activity -- NODE_URL FROM TO`.

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let node = zchan_tx::node::Node::new(a[1].clone());
    let (from, to): (u32, u32) = (a[2].parse().unwrap(), a[3].parse().unwrap());
    let t0 = std::time::Instant::now();
    let (s0, s1) = (zchan_tx::notes::tree_size(&node, from).await.unwrap(), zchan_tx::notes::tree_size(&node, to).await.unwrap());
    println!("{from}..{to}: {} commitments added ({:?})", s1 - s0, t0.elapsed());
}
