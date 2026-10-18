// okcode-access has no golden functions; the contract is tested in tests/contract.rs.
#[test]
fn golden_none() {
    assert!(okcode_access::golden_call("x", &[]).is_err());
    println!("golden: none (no goldens for okcode-access)");
}
