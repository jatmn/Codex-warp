use super::*;

#[test]
fn generated_id_uses_prefix_process_and_unique_sequence() {
    let first = generated_id("resp");
    let second = generated_id("resp");
    let process_prefix = format!("resp_{}_", std::process::id());

    assert!(first.starts_with(&process_prefix));
    assert!(second.starts_with(&process_prefix));
    assert_ne!(first, second);
    assert_eq!(first.split('_').count(), 4);
}
