fn main() {
    let metadata = reticulum_rns_rete::metadata();
    let _probe = reticulum_rns_rete::new_conformance_node(&[0x52; 32])
        .expect("the deterministic host-only probe node should construct");

    println!("candidate={}", metadata.id);
    println!("source={}", metadata.source);
    println!("revision={}", metadata.revision);
    println!("license={}", metadata.license);
    println!("accepted={}", metadata.accepted);
    println!(
        "capacities=paths:{},announces:{},dedup:{},links:{}",
        reticulum_rns_rete::probe_capacity::PATHS,
        reticulum_rns_rete::probe_capacity::ANNOUNCES,
        reticulum_rns_rete::probe_capacity::DEDUPLICATION_ENTRIES,
        reticulum_rns_rete::probe_capacity::LINKS,
    );
    println!("status=scaffold-only; see docs/phase-0-acceptance.md");
}
