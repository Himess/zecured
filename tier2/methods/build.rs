use std::collections::HashMap;

use risc0_build::{DockerOptionsBuilder, GuestOptionsBuilder, embed_methods, embed_methods_with_options};

/// `ZECURED_DOCKER_GUESTS=1` builds both guests in RISC Zero's pinned guest-builder image with
/// `tier2/` as the build context: the reproducible build whose image ids are deployed (review
/// E4, docs/TIER2.md § 8.1). Without it, the guests are built with the local risc0 toolchain,
/// whose image ids depend on the machine.
fn main() {
    println!("cargo:rerun-if-env-changed=ZECURED_DOCKER_GUESTS");
    if std::env::var_os("ZECURED_DOCKER_GUESTS").is_none() {
        embed_methods();
        return;
    }
    // Build scripts run in the package directory (tier2/methods): the context is tier2/, which
    // holds the guests, the statement crate and the vendored pasta_curves.
    // The image's Rust (1.97) is the local risc0 toolchain's; the tag is pinned here rather than
    // left to risc0-build's default, so the build is fully specified by this repository.
    let docker = DockerOptionsBuilder::default()
        .root_dir("..")
        .docker_container_tag("r0.1.97.0")
        .build()
        .expect("docker options");
    let options = GuestOptionsBuilder::default().use_docker(docker).build().expect("guest options");
    embed_methods_with_options(HashMap::from([("deposit", options.clone()), ("violation", options)]));
}
