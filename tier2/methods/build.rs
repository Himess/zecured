use std::collections::HashMap;

use risc0_build::{DockerOptionsBuilder, GuestOptionsBuilder, embed_methods_with_options};

/// Builds the three guests for the network this crate is built for (design notes T-09: a build
/// parameter, never a witness field): Testnet by default, Mainnet with the `mainnet` feature,
/// which reaches the guests as their own `mainnet` feature and the statements'. Each network's
/// guests have their own image ids (deposit-test, violation-test, payment-test, deposit-main,
/// violation-main, payment-main). The payment guest (Zecured Swap) is built with the same options
/// as the other two, and adding it changes nothing in theirs: each guest is its own workspace with
/// its own Cargo.lock, compiled alone (the `guests` workflow checks that the deposit and violation
/// ids are still the deployed ones).
///
/// `ZECURED_DOCKER_GUESTS=1` builds the guests in RISC Zero's pinned guest-builder image with
/// `tier2/` as the build context: the reproducible build whose image ids are deployed (review
/// E4, docs/TIER2.md § 8.1). Without it, the guests are built with the local risc0 toolchain,
/// whose image ids depend on the machine.
fn main() {
    println!("cargo:rerun-if-env-changed=ZECURED_DOCKER_GUESTS");
    let features = if std::env::var_os("CARGO_FEATURE_MAINNET").is_some() {
        vec!["mainnet".to_owned()]
    } else {
        vec![]
    };
    let mut options = GuestOptionsBuilder::default();
    options.features(features);
    if std::env::var_os("ZECURED_DOCKER_GUESTS").is_some() {
        // Build scripts run in the package directory (tier2/methods): the context is tier2/, which
        // holds the guests, the statement crates and the vendored pasta_curves.
        // The image's Rust (1.97) is the local risc0 toolchain's; the tag is pinned here rather than
        // left to risc0-build's default, so the build is fully specified by this repository.
        let docker = DockerOptionsBuilder::default()
            .root_dir("..")
            .docker_container_tag("r0.1.97.0")
            .build()
            .expect("docker options");
        options.use_docker(docker);
    }
    let options = options.build().expect("guest options");
    embed_methods_with_options(HashMap::from([
        ("deposit", options.clone()),
        ("violation", options.clone()),
        ("payment", options),
    ]));
}
