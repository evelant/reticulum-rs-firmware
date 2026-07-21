use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    ffi::OsStr,
    fs,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};

use quote::ToTokens;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use syn::{Fields, ImplItem, Item, Type, Visibility};

mod e290_authenticated_usb;
mod e290_pairing_control;
mod e290_pairing_live;
mod e290_rns_inbox_fixture;
mod e290_runtime_measurement;
mod phase1_closure;
mod phase1_hil;
mod phase1_image;
mod phase1_powered_evidence;
mod phase1_source;
mod phase1_tooling;

const HOST_RUST: &str = "rustc 1.97.0";
const ESP_RUST_FINGERPRINT: &str = "1.95.0.0";
const ESPFLASH_VERSION: &str = "espflash 4.5.0";

fn main() -> ExitCode {
    let mut args = env::args().skip(1);
    match args.next().as_deref() {
        Some("doctor") if args.next().is_none() => doctor(),
        Some("build-tracker") if args.next().is_none() => build_tracker(),
        Some("e290-pairing-control") => e290_pairing_control::run(args.collect()),
        Some("e290-pairing-live") => e290_pairing_live::run(args.collect()),
        Some("e290-authenticated-usb") => e290_authenticated_usb::run(args.collect()),
        Some("e290-rns-inbox-fixture") => e290_rns_inbox_fixture::run(args.collect()),
        Some("e290-runtime-measurement") => e290_runtime_measurement::run(args.collect()),
        Some("check-rns-vectors") if args.next().is_none() => check_rns_vectors(),
        Some("check-rnode-hil-vectors") if args.next().is_none() => check_rnode_hil_vectors(),
        Some("graph-policy") if args.next().is_none() => graph_policy(),
        Some("rx-api-policy") if args.next().is_none() => rx_api_policy(),
        Some("print-rx-api-surface") if args.next().is_none() => print_rx_api_surface(),
        Some("phase1-rx-hil-artifacts") => phase1_hil::run(args.collect(), &workspace_root()),
        Some("phase1-rx-closure-artifacts") => {
            phase1_closure::run(args.collect(), &workspace_root())
        }
        Some("phase1-rx-powered-evidence") => {
            phase1_powered_evidence::run(args.collect(), &workspace_root())
        }
        _ => {
            eprintln!(
                "usage: cargo run -p xtask -- \
                 <doctor|build-tracker|e290-pairing-control|e290-pairing-live|e290-authenticated-usb|e290-rns-inbox-fixture|e290-runtime-measurement|check-rns-vectors|check-rnode-hil-vectors|graph-policy|rx-api-policy|print-rx-api-surface|phase1-rx-hil-artifacts|phase1-rx-closure-artifacts|phase1-rx-powered-evidence>"
            );
            ExitCode::from(2)
        }
    }
}

fn check_rnode_hil_vectors() -> ExitCode {
    let root = workspace_root();
    let python = env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let generator = root.join("interop/python/generate_rnode_hil_vectors.py");
    let mut python_paths = vec![root.join("interop/python")];
    if let Some(existing) = env::var_os("PYTHONPATH") {
        python_paths.extend(env::split_paths(&existing));
    }
    let python_path = match env::join_paths(python_paths) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("could not construct the RNode HIL Python path: {error}");
            return ExitCode::FAILURE;
        }
    };

    let vector_status = Command::new(&python)
        .current_dir(&root)
        .arg(generator)
        .arg("--check")
        .status();
    match vector_status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("RNode HIL vector check exited with {status}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("could not run RNode HIL vector generator: {error}");
            return ExitCode::FAILURE;
        }
    }

    let test_status = Command::new(&python)
        .current_dir(&root)
        .args([
            "-m",
            "unittest",
            "discover",
            "-s",
            "interop/python",
            "-p",
            "test_*.py",
            "-v",
        ])
        .env("PYTHONPATH", python_path)
        .status();
    match test_status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("RNode HIL peer tests exited with {status}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("could not run RNode HIL peer tests: {error}");
            return ExitCode::FAILURE;
        }
    }

    run_inherited_at(
        "cargo",
        &[
            "test",
            "--locked",
            "-p",
            "reticulum-rns-rete-rx",
            "--test",
            "rnode_hil_vectors",
        ],
        &root,
    )
}

fn check_rns_vectors() -> ExitCode {
    let root = workspace_root();
    let python = env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let script = root.join("interop/python/generate_rns_vectors.py");
    let tests = root.join("interop/python/test_rns_vectors.py");

    let vector_status = Command::new(&python)
        .current_dir(&root)
        .arg(script)
        .arg("--check")
        .status();
    match vector_status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("Python vector check exited with {status}");
            eprintln!(
                "hint: set PYTHON to an environment containing \
                 interop/python/requirements-rns-1.3.8.txt"
            );
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("could not run Python vector generator: {error}");
            return ExitCode::FAILURE;
        }
    }

    let test_status = Command::new(&python).current_dir(&root).arg(tests).status();
    match test_status {
        Ok(status) if status.success() => {}
        Ok(status) => {
            eprintln!("released Python RNS vector tests exited with {status}");
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("could not run released Python RNS vector tests: {error}");
            return ExitCode::FAILURE;
        }
    }

    run_inherited_at(
        "cargo",
        &["run", "--locked", "-p", "reticulum-conformance-rete"],
        &root,
    )
}

fn doctor() -> ExitCode {
    let checks: &[(&str, &str, &[&str], &str)] = &[
        ("host Rust", "rustc", &["--version"], HOST_RUST),
        (
            "ESP Rust",
            "rustc",
            &["+esp", "--version"],
            ESP_RUST_FINGERPRINT,
        ),
        ("espflash", "espflash", &["--version"], ESPFLASH_VERSION),
        (
            "Xtensa GCC",
            "xtensa-esp32s3-elf-gcc",
            &["--version"],
            "xtensa-esp-elf",
        ),
    ];

    let mut failed = false;
    for (label, program, args, expected) in checks {
        match capture(program, *args) {
            Ok(output) if output.contains(expected) => {
                println!("ok: {label}: {}", first_line(&output));
            }
            Ok(output) => {
                eprintln!(
                    "error: {label}: expected {expected:?}, got {:?}",
                    first_line(&output)
                );
                failed = true;
            }
            Err(error) => {
                eprintln!("error: {label}: {error}");
                failed = true;
            }
        }
    }

    if failed {
        eprintln!("hint: install the pinned toolchain with espup, then source ~/export-esp.sh");
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn build_tracker() -> ExitCode {
    if capture("xtensa-esp32s3-elf-gcc", ["--version"]).is_err() {
        eprintln!("Xtensa GCC is not on PATH; source ~/export-esp.sh first");
        return ExitCode::FAILURE;
    }

    run_inherited(
        "cargo",
        &[
            "+esp",
            "build",
            "--locked",
            "--release",
            "-p",
            "reticulum-heltec-tracker-v2",
            "--target",
            "xtensa-esp32s3-none-elf",
        ],
    )
}

fn graph_policy() -> ExitCode {
    let root = workspace_root();
    if let Err(error) = validate_pairing_publication_workspace(&root) {
        eprintln!("error: credential-store integration source boundary: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = validate_e290_inbox_commit_fault_hil_workspace(&root) {
        eprintln!("error: E290 inbox commit-fault HIL source boundary: {error}");
        return ExitCode::FAILURE;
    }
    if let Err(error) = validate_e290_runtime_measurement_hil_workspace(&root) {
        eprintln!("error: E290 runtime-measurement HIL source boundary: {error}");
        return ExitCode::FAILURE;
    }
    let mut product_graphs = Vec::new();
    for feature in [
        "safe-idle",
        "lab-rx",
        "lab-rx-backpressure",
        "lab-rx-electrical-hil",
        "lab-rx-returned-fault-hil",
        "lab-rx-reset-journal-corrupt-hil",
        "lab-rx-reset-journal-torn-hil",
    ] {
        match capture(
            "cargo",
            [
                "tree",
                "--locked",
                "-p",
                "reticulum-heltec-tracker-v2",
                "--no-default-features",
                "--features",
                feature,
                "--target",
                "all",
            ],
        ) {
            Ok(tree) => product_graphs.push((feature, tree)),
            Err(error) => {
                eprintln!("error: could not inspect product {feature} graph: {error}");
                return ExitCode::FAILURE;
            }
        }
    }
    let all_features_product = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-tracker-v2",
            "--all-features",
            "--target",
            "all",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect product all-features graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let storage_hil = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-tracker-v2-storage-hil",
            "--target",
            "all",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect physical-storage HIL graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let tx_hil = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-tracker-v2-tx-hil",
            "--target",
            "all",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect hazardous TX HIL graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let semantic_tx_hil = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-tracker-v2-tx-hil",
            "--no-default-features",
            "--features",
            "semantic-announce-hil,tracker-radio",
            "--target",
            "all",
            "--format",
            "{p} features=[{f}]",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect semantic announce TX HIL graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let semantic_roundtrip_tx_hil = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-tracker-v2-tx-hil",
            "--no-default-features",
            "--features",
            "semantic-roundtrip-hil,tracker-radio",
            "--target",
            "all",
            "--format",
            "{p} features=[{f}]",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect semantic round-trip TX HIL graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let e290_semantic_hil = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-vision-master-e290-semantic-hil",
            "--target",
            "all",
            "--format",
            "{p} features=[{f}]",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect E290 semantic HIL graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let e290_node = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-vision-master-e290-node",
            "--target",
            "all",
            "--format",
            "{p} features=[{f}]",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect permanent E290 node graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let e290_inbox_commit_fault_hil = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-vision-master-e290-node",
            "--no-default-features",
            "--features",
            "rns-inbox-commit-fault-hil",
            "--target",
            "all",
            "--format",
            "{p} features=[{f}]",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect E290 inbox commit-fault HIL graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let e290_runtime_measurement_hil = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-heltec-vision-master-e290-node",
            "--no-default-features",
            "--features",
            "runtime-measurement-hil",
            "--target",
            "all",
            "--format",
            "{p} features=[{f}]",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect E290 runtime-measurement HIL graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let comparison = match capture(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-rns-leviculum",
            "--target",
            "all",
        ],
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect comparison graph: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut interface_neutral_rns_closures = Vec::new();
    for (label, package) in [
        ("Rete integration", "reticulum-rns-rete"),
        ("node core", "reticulum-node-core"),
    ] {
        match capture_stdout_at(
            "cargo",
            [
                "tree",
                "--locked",
                "-p",
                package,
                "--edges",
                "normal",
                "--target",
                "all",
                "--prefix",
                "none",
                "--no-dedupe",
                "--format",
                "{p}",
            ],
            &root,
        ) {
            Ok(tree) => interface_neutral_rns_closures.push((label, tree)),
            Err(error) => {
                eprintln!("error: could not inspect {label} normal closure: {error}");
                return ExitCode::FAILURE;
            }
        }
    }

    // Workspace metadata exposes workspace-unified feature sets. This package-selected
    // tree supplies the dispatcher's own target-all normal closure and feature sets.
    let radio_tx_dispatch_closure = match capture_stdout_at(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-radio-tx-dispatch",
            "--edges",
            "normal",
            "--target",
            "all",
            "--prefix",
            "none",
            "--no-dedupe",
            "--format",
            "{p}\t{f}",
        ],
        &root,
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect real radio TX dispatcher closure: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Use the actual generic bare-metal target rather than target-all feature
    // unification: this is the allocation-free closure shipped by the portable
    // wire crate, including its exact cryptographic feature selections.
    let lxmf_wire_closure = match capture_stdout_at(
        "cargo",
        [
            "tree",
            "--locked",
            "-p",
            "reticulum-lxmf-wire",
            "--edges",
            "normal",
            "--target",
            "riscv32imac-unknown-none-elf",
            "--prefix",
            "none",
            "--no-dedupe",
            "--format",
            "{p}\t{f}",
        ],
        &root,
    ) {
        Ok(tree) => tree,
        Err(error) => {
            eprintln!("error: could not inspect LXMF wire generic bare-metal closure: {error}");
            return ExitCode::FAILURE;
        }
    };

    let mut failed = false;
    for (feature, product) in &product_graphs {
        if let Err(error) = validate_product_graph_boundary(feature, product) {
            eprintln!("error: {error}");
            failed = true;
        }
        let returned_fault_hook = product.contains("reticulum-lab-rx-returned-fault-hil");
        if returned_fault_hook != (*feature == "lab-rx-returned-fault-hil") {
            eprintln!(
                "error: product {feature} returned-fault hook presence is {returned_fault_hook}, expected {}",
                *feature == "lab-rx-returned-fault-hil"
            );
            failed = true;
        }
    }
    if let Err(error) = validate_product_graph_boundary("all-features", &all_features_product) {
        eprintln!("error: {error}");
        failed = true;
    }
    if let Err(error) = validate_storage_hil_graph_boundary(&storage_hil) {
        eprintln!("error: {error}");
        failed = true;
    }
    if let Err(error) = validate_tx_hil_graph_boundary(&tx_hil) {
        eprintln!("error: {error}");
        failed = true;
    }
    if let Err(error) = validate_semantic_tx_hil_graph_boundary(&semantic_tx_hil) {
        eprintln!("error: {error}");
        failed = true;
    }
    if let Err(error) =
        validate_semantic_roundtrip_tx_hil_graph_boundary(&semantic_roundtrip_tx_hil)
    {
        eprintln!("error: {error}");
        failed = true;
    }
    if let Err(error) = validate_e290_semantic_hil_graph_boundary(&e290_semantic_hil) {
        eprintln!("error: {error}");
        failed = true;
    }
    if let Err(error) = validate_e290_node_graph_boundary(&e290_node) {
        eprintln!("error: {error}");
        failed = true;
    }
    if let Err(error) = validate_e290_inbox_commit_fault_hil_graph_boundary(
        &e290_node,
        &e290_inbox_commit_fault_hil,
    ) {
        eprintln!("error: {error}");
        failed = true;
    }
    if let Err(error) = validate_e290_runtime_measurement_hil_graph_boundary(
        &e290_node,
        &e290_runtime_measurement_hil,
    ) {
        eprintln!("error: {error}");
        failed = true;
    }
    for forbidden in ["rete-core", "rete-stack", "rete-transport", "rete-lxmf"] {
        if cargo_tree_contains_package(&comparison, forbidden) {
            eprintln!("error: Leviculum comparison graph contains forbidden {forbidden}");
            failed = true;
        }
    }
    for (label, closure) in &interface_neutral_rns_closures {
        if let Err(error) = validate_interface_neutral_rns_closure(label, closure) {
            eprintln!("error: {error}");
            failed = true;
        }
    }

    let candidate = reticulum_rns_rete::metadata();
    let resolved = capture_stdout_at(
        "cargo",
        ["metadata", "--locked", "--format-version", "1"],
        &root,
    )
    .and_then(|json| {
        validate_pairing_publication_workspace_member_coverage(&json, &root)
            .map_err(|error| format!("credential-store integration scan coverage: {error}"))?;
        validate_resolved_rete_pin(&json, candidate.source, candidate.revision)
            .map_err(|error| format!("Rete pin/report mismatch: {error}"))?;
        validate_resolved_esp_rtos_patch(&json, &root)
            .map_err(|error| format!("esp-rtos patch boundary: {error}"))?;
        validate_resolved_lora_phy_patch(&json, &root)
            .map_err(|error| format!("lora-phy patch boundary: {error}"))?;
        validate_firmware_dependency_boundary(&json, &root)
            .map_err(|error| format!("firmware receive-only dependency boundary: {error}"))?;
        validate_portable_layer_dependency_boundary(&json, &root)
            .map_err(|error| format!("portable layer dependency boundary: {error}"))?;
        validate_device_api_edge_dependency_boundary(&json, &root)
            .map_err(|error| format!("device API edge dependency boundary: {error}"))?;
        validate_portable_durability_dependency_boundaries(&json, &root)
            .map_err(|error| format!("portable durability dependency boundary: {error}"))?;
        validate_rns_inbox_store_dependency_boundary(&json, &root)
            .map_err(|error| format!("durable RNS inbox store dependency boundary: {error}"))?;
        validate_tracker_radio_dependency_boundary(&json, &root)
            .map_err(|error| format!("Tracker bidirectional radio dependency boundary: {error}"))?;
        validate_tx_handoff_dependency_boundary(&json, &root)
            .map_err(|error| format!("TX handoff dependency boundary: {error}"))?;
        validate_interface_router_dependency_boundary(&json, &root)
            .map_err(|error| format!("interface router dependency boundary: {error}"))?;
        validate_tx_dispatch_dependency_boundary(&json, &root)
            .map_err(|error| format!("TX dispatcher dependency boundary: {error}"))?;
        validate_radio_interface_dependency_boundary(&json, &root)
            .map_err(|error| format!("radio interface dependency boundary: {error}"))?;
        validate_e290_board_facts_dependency_boundary(&json, &root)
            .map_err(|error| format!("E290 board-facts dependency boundary: {error}"))?;
        validate_semantic_hil_dependency_boundary(&json, &root)
            .map_err(|error| format!("semantic HIL fixture dependency boundary: {error}"))?;
        validate_lora_phy_radio_dependency_boundary(&json, &root)
            .map_err(|error| format!("shared lora-phy radio dependency boundary: {error}"))?;
        validate_e290_radio_dependency_boundary(&json, &root)
            .map_err(|error| format!("E290 radio dependency boundary: {error}"))?;
        validate_e290_node_feature_boundary(&json, &root)
            .map_err(|error| format!("permanent E290 node composition boundary: {error}"))?;
        validate_radio_tx_dispatch_dependency_boundary(&json, &root)
            .map_err(|error| format!("real radio TX dispatcher dependency boundary: {error}"))?;
        validate_radio_tx_dispatch_resolved_closure(&json, &radio_tx_dispatch_closure, &root)
            .map_err(|error| format!("real radio TX dispatcher resolved closure: {error}"))?;
        validate_lxmf_wire_dependency_boundary(&json, &root)
            .map_err(|error| format!("LXMF wire dependency boundary: {error}"))?;
        validate_lxmf_wire_resolved_closure(&json, &lxmf_wire_closure, &root)
            .map_err(|error| format!("LXMF wire generic bare-metal closure: {error}"))?;
        validate_storage_model_dependency_boundary(&json, &root)
            .map_err(|error| format!("durable storage model dependency boundary: {error}"))?;
        validate_storage_journal_dependency_boundary(&json, &root)
            .map_err(|error| format!("physical storage journal dependency boundary: {error}"))?;
        validate_storage_actor_dependency_boundary(&json, &root)
            .map_err(|error| format!("sole storage actor dependency boundary: {error}"))?;
        validate_submission_runtime_dependency_boundary(&json, &root)
            .map_err(|error| format!("durable submission runtime dependency boundary: {error}"))?;
        validate_device_api_adapter_dependency_boundary(&json, &root)
            .map_err(|error| format!("device API adapter dependency boundary: {error}"))?;
        validate_storage_hil_dependency_boundary(&json, &root)
            .map_err(|error| format!("physical storage HIL dependency boundary: {error}"))?;
        validate_submission_projector_dependency_boundary(&json, &root)
            .map_err(|error| format!("submission projector dependency boundary: {error}"))?;
        validate_tx_supervisor_dependency_boundary(&json, &root)
            .map_err(|error| format!("TX supervisor dependency boundary: {error}"))
    });
    if let Err(error) = resolved {
        eprintln!("error: {error}");
        failed = true;
    }

    if failed {
        ExitCode::FAILURE
    } else {
        println!(
            "ok: all safe, RX, HIL and all-features all-target product graphs, the RF-inert physical-storage HIL graph, the separately hazardous default-sentinel, Tracker semantic-announce/semantic-round-trip and E290 semantic-round-trip TX HIL graphs, the permanent, inbox commit-fault and runtime-measurement E290 node graphs and the Leviculum \
             comparison graph are isolated; the returned-radio-fault, inbound-commit-fault and runtime-measurement hooks are feature-exclusive; \
             legacy Tracker firmware direct dependencies use only the RX façade and every-feature resolution \
             excludes TX ownership and pre-integration durable crates; resolved Rete packages match reported \
             source/revision; esp-rtos and lora-phy resolve only to their reviewed local patches, \
             and each checked vendor inventory reconstructs the pristine registry source; the device \
             API and node core remain mutually isolated and free of direct platform dependencies; the \
             allocation-free device-API framing crate has only its reviewed zeroization edge and no feature surface; the featureless pre-authentication pairing-control codec reaches only framing, remains absent from every legacy/HIL graph and is composed only into the permanent E290 USB control surface; the live-pairing core has only its reviewed HMAC/SHA-256/zeroization, credential-authority and framing edges plus test-only hex, is composed only into the permanent E290 resident credential lifecycle, and remains absent from every legacy/HIL graph, while the \
             boot-lifetime job handoff reaches only the logical device API and Embassy Sync, and the \
             credential authority has only its exact logical device-API, constant-time comparison and zeroization edges; credential-store integration escape identifiers remain restricted to their exact reviewed definition and call sites in the two trusted owner files, and every workspace member target remains beneath a scanned source root; the physical-presence pairing policy has only its exact feature-disabled credential-authority edge, is composed feature-free only into the permanent E290 node, and remains absent from every legacy product and HIL graph; the \
             authenticated session layer has only its exact reviewed cryptographic, device-API, credentials, framing and handoff normal edges plus its exact test-only hex, semantic-adapter and storage-model fixtures; \
             the Rete integration and node-core normal closures contain no RNode, radio-interface, LoRa or board package; \
             the shared lora-phy owner and E290 radio wrapper have only their exact reviewed HAL, framing, board and test edges; \
             the Tracker bidirectional radio has only its reviewed board, shared lora-phy owner, framing, HAL, critical-section and patched lora-phy edges while the historical board TX-HIL crate is a one-edge compatibility facade; the E290 and Tracker semantic HILs share one board-independent fixture crate while retaining separate physical MAC and radio authorization, and the E290 graph cannot reach Tracker firmware, board, radio, FEM or runtime dependencies; the permanent E290 node reaches the LoRa-first node/router/dispatcher graph, exact portable identity, credential-store authority, announce-clock, NOR-region, durable-submission and durable inbound-RNS-inbox layers, both target-safe experimental device-API semantic ports, the featureless framed USB pre-authentication control codec, the resident live-pairing lifecycle and a minimal boot-lifetime USB authenticated-session bearer with transport-neutral admission and node-side dispatch while excluding onboard clients and foreign Tracker/HIL packages; \
             the interface router has only its reviewed node-core and Embassy Sync normal edges plus test-only rand_core and RNS fixture edges; \
             the TX handoff, RF-inert dispatcher and supervisor use only their reviewed node-core, \
             interface-router ingress, handoff, dispatcher, Embassy Sync/Futures/Time, rand_core \
             and SHA-256 dependency edges; radio-interface has only its exact lora-modulation, RNS-conformance and \
             test-only Embassy Sync edges; the E290 board-facts crate has only its one \
             feature-free local radio-interface edge; the staged real-radio dispatcher has only its reviewed \
             portable interface-router, node-core, handoff, radio-interface, Embassy Sync/Time and rand_core normal \
             edges plus test-only Embassy Futures and static storage, and its target-all normal \
             closure exactly matches all 64 reviewed local \
             path or registry/Git source identities and dispatcher-specific enabled-feature sets; \
             the bounded LXMF wire crate has only its five reviewed default-feature-disabled cryptographic \
             normal edges, the single reviewed streaming-verification feature and three host-test edges, and its generic bare-metal normal closure exactly \
             matches the reviewed registry identities without std, alloc, Rete, platform, radio or storage packages; \
             the portable identity, announce-clock and NOR-region crates use only their exact reviewed embedded-storage, rand_core, SHA-256 and zeroize subsets; the durable inbound RNS inbox store uses only exact feature-free embedded-storage and SHA-256 pins; the durable \
             storage model uses only reviewed minicbor and SHA-256 edges; \
             the physical storage journal uses only reviewed embedded-storage, storage-model and SHA-256 edges; \
             the sole storage actor uses only reviewed embedded-storage, node-core, journal, semantic-model and submission-projector edges plus its reviewed test-only rand_core edge; the durable submission runtime uses only reviewed Embassy Sync, embedded-storage, rand_core, node-core, storage-actor, semantic-model, submission-projector and transport-neutral supervisor edges plus its journal-only test fixture; \
             the device API adapter uses only reviewed device-API and semantic-model normal edges with test-only embedded-storage and storage-actor fixtures plus exact experimental-rns-data and experimental-rns-inbox feature forwards; \
             the physical-storage HIL has only its reviewed raw-flash, journal, semantic-model, logging and ESP runtime edges and no radio/protocol stack; \
             the submission projector uses only reviewed node-core and storage-model \
             and test-only rand_core and RNS adapter edges"
        );
        ExitCode::SUCCESS
    }
}

const PAIRING_PUBLICATION_SCAN_ROOTS: [&str; 5] =
    ["comparisons", "crates", "firmware", "tools", "xtask"];

struct TrustedIdentifierExpectation {
    identifier: &'static str,
    expected_occurrences_by_source: [usize; CREDENTIAL_STORE_INTEGRATION_ALLOWED_SOURCES.len()],
}

const CREDENTIAL_STORE_INTEGRATION_ESCAPE_EXPECTATIONS: [TrustedIdentifierExpectation; 3] = [
    TrustedIdentifierExpectation {
        identifier: concat!("credential_store_", "integration"),
        expected_occurrences_by_source: [1, 1],
    },
    TrustedIdentifierExpectation {
        identifier: concat!("into_unpublished_authority_", "for_store_unchecked"),
        expected_occurrences_by_source: [2, 1],
    },
    TrustedIdentifierExpectation {
        identifier: concat!("select_pending_for_proof_", "for_store_unchecked"),
        expected_occurrences_by_source: [2, 1],
    },
];

const CREDENTIAL_STORE_INTEGRATION_ALLOWED_SOURCES: [&str; 2] = [
    "crates/device-api-credentials/src/lib.rs",
    "crates/device-api-credential-store/src/lib.rs",
];

fn validate_pairing_publication_workspace(root: &Path) -> Result<(), String> {
    let mut sources = Vec::new();
    for relative in PAIRING_PUBLICATION_SCAN_ROOTS {
        let directory = root.join(relative);
        if directory.is_dir() {
            collect_workspace_rust_sources(root, &directory, &mut sources)?;
        }
    }
    validate_pairing_publication_sources(&sources)
}

fn collect_workspace_rust_sources(
    root: &Path,
    directory: &Path,
    sources: &mut Vec<(String, String)>,
) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("could not read {}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("could not enumerate {}: {error}", directory.display()))?;
    entries.sort_by_key(std::fs::DirEntry::file_name);

    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "workspace Rust source tree contains unsupported symlink {}",
                path.display()
            ));
        }
        if file_type.is_dir() {
            collect_workspace_rust_sources(root, &path, sources)?;
            continue;
        }
        if !file_type.is_file() || path.extension() != Some(OsStr::new("rs")) {
            continue;
        }

        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("could not relativize {}: {error}", path.display()))?
            .to_str()
            .ok_or_else(|| format!("workspace source path {} is not UTF-8", path.display()))?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let source = fs::read_to_string(&path)
            .map_err(|error| format!("could not read {}: {error}", path.display()))?;
        sources.push((relative, source));
    }
    Ok(())
}

fn validate_pairing_publication_sources(sources: &[(String, String)]) -> Result<(), String> {
    for (path, source) in sources {
        if CREDENTIAL_STORE_INTEGRATION_ALLOWED_SOURCES.contains(&path.as_str()) {
            continue;
        }
        for expectation in &CREDENTIAL_STORE_INTEGRATION_ESCAPE_EXPECTATIONS {
            if source.contains(expectation.identifier) {
                return Err(format!(
                    "trusted identifier {:?} appears outside the two credential-store owner files in {path}",
                    expectation.identifier
                ));
            }
        }
    }

    for expectation in &CREDENTIAL_STORE_INTEGRATION_ESCAPE_EXPECTATIONS {
        for (source_index, path) in CREDENTIAL_STORE_INTEGRATION_ALLOWED_SOURCES
            .iter()
            .enumerate()
        {
            let mut matching_sources = sources.iter().filter(|(candidate, _)| candidate == path);
            let (_, source) = matching_sources.next().ok_or_else(|| {
                format!("required credential-store integration owner source {path} was not scanned")
            })?;
            if matching_sources.next().is_some() {
                return Err(format!(
                    "credential-store integration owner source {path} was scanned more than once"
                ));
            }

            let actual = source.matches(expectation.identifier).count();
            let expected = expectation.expected_occurrences_by_source[source_index];
            if actual != expected {
                return Err(format!(
                    "trusted identifier {:?} has {actual} occurrences in {path}; expected exactly {expected} reviewed definition/use occurrences",
                    expectation.identifier
                ));
            }
        }
    }
    Ok(())
}

fn validate_pairing_publication_workspace_member_coverage(
    metadata_json: &str,
    root: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let workspace_members = metadata["workspace_members"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no workspace_members array".to_owned())?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;

    for member in workspace_members {
        let member_id = member
            .as_str()
            .ok_or_else(|| "cargo metadata contains a non-string workspace member ID".to_owned())?;
        let package = packages
            .iter()
            .find(|package| package["id"].as_str() == Some(member_id))
            .ok_or_else(|| format!("workspace member {member_id:?} has no package metadata"))?;
        let package_name = package["name"].as_str().unwrap_or(member_id);
        let manifest = package["manifest_path"]
            .as_str()
            .ok_or_else(|| format!("workspace member {package_name:?} has no manifest_path"))?;
        validate_pairing_publication_scanned_path(root, Path::new(manifest), package_name)?;

        let targets = package["targets"]
            .as_array()
            .ok_or_else(|| format!("workspace member {package_name:?} has no targets array"))?;
        if targets.is_empty() {
            return Err(format!(
                "workspace member {package_name:?} has no source targets to verify"
            ));
        }
        for target in targets {
            let source = target["src_path"].as_str().ok_or_else(|| {
                format!("workspace member {package_name:?} has a target without src_path")
            })?;
            validate_pairing_publication_scanned_path(root, Path::new(source), package_name)?;
        }
    }
    Ok(())
}

fn validate_pairing_publication_scanned_path(
    root: &Path,
    path: &Path,
    package_name: &str,
) -> Result<(), String> {
    let relative = path.strip_prefix(root).map_err(|_| {
        format!(
            "workspace member {package_name:?} source path {} is outside the workspace scan boundary {}",
            path.display(),
            root.display()
        )
    })?;
    let scan_root = match relative.components().next() {
        Some(Component::Normal(component)) => component,
        _ => {
            return Err(format!(
                "workspace member {package_name:?} source path {} has no scannable workspace root",
                path.display()
            ));
        }
    };
    if !PAIRING_PUBLICATION_SCAN_ROOTS
        .iter()
        .any(|allowed| scan_root == OsStr::new(allowed))
    {
        return Err(format!(
            "workspace member {package_name:?} source path {} is under unscanned root {:?}; allowed source roots are {:?}",
            path.display(),
            scan_root,
            PAIRING_PUBLICATION_SCAN_ROOTS
        ));
    }
    Ok(())
}

const INTERFACE_NEUTRAL_RNS_FORBIDDEN: [&str; 9] = [
    "lora-modulation",
    "lora-phy",
    "reticulum-board-heltec-tracker-v2",
    "reticulum-board-heltec-vision-master-e290",
    "reticulum-radio-interface",
    "reticulum-radio-lora-phy",
    "reticulum-radio-tx-dispatch",
    "reticulum-rns-rete-rx",
    "reticulum-heltec-tracker-v2",
];

fn validate_interface_neutral_rns_closure(label: &str, tree: &str) -> Result<(), String> {
    for forbidden in INTERFACE_NEUTRAL_RNS_FORBIDDEN {
        if cargo_tree_contains_package(tree, forbidden) {
            return Err(format!(
                "{label} normal closure contains interface-specific package {forbidden}"
            ));
        }
    }
    Ok(())
}

fn cargo_tree_contains_package(tree: &str, package: &str) -> bool {
    tree.lines()
        .any(|line| line.split_whitespace().any(|field| field == package))
}

const PRODUCT_GRAPH_FORBIDDEN: [&str; 27] = [
    "leviculum-core",
    "rete-lxmf",
    "lxmf-rs",
    "reticulum-board-heltec-tracker-v2-radio",
    "reticulum-board-heltec-tracker-v2-tx-hil",
    "reticulum-board-heltec-vision-master-e290-radio",
    "reticulum-device-api-adapter",
    "reticulum-device-api-credential-store",
    "reticulum-device-api-credentials",
    "reticulum-device-api-framing",
    "reticulum-device-api-pairing-control",
    "reticulum-device-api-pairing",
    "reticulum-device-api-handoff",
    "reticulum-device-api-pairing-policy",
    "reticulum-device-api-session",
    "reticulum-node-core",
    "reticulum-radio-tx-dispatch",
    "reticulum-radio-lora-phy",
    "reticulum-rns-inbox-store",
    "reticulum-semantic-roundtrip-hil",
    "reticulum-storage-actor",
    "reticulum-storage-journal",
    "reticulum-storage-model",
    "reticulum-submission-projector",
    "reticulum-tx-dispatch",
    "reticulum-tx-handoff",
    "reticulum-tx-supervisor",
];

fn validate_product_graph_boundary(label: &str, tree: &str) -> Result<(), String> {
    for forbidden in PRODUCT_GRAPH_FORBIDDEN {
        if cargo_tree_contains_package(tree, forbidden) {
            return Err(format!(
                "product {label} all-target graph contains forbidden {forbidden}"
            ));
        }
    }
    Ok(())
}

const STORAGE_HIL_GRAPH_FORBIDDEN: [&str; 32] = [
    "embassy-executor",
    "esp-radio",
    "lora-modulation",
    "reticulum-radio-lora-phy",
    "lora-phy",
    "rete-core",
    "rete-stack",
    "rete-transport",
    "reticulum-board-heltec-tracker-v2-radio",
    "reticulum-board-heltec-tracker-v2-tx-hil",
    "reticulum-board-heltec-tracker-v2",
    "reticulum-board-heltec-vision-master-e290-radio",
    "reticulum-device-api-adapter",
    "reticulum-device-api-credential-store",
    "reticulum-device-api-credentials",
    "reticulum-device-api-framing",
    "reticulum-device-api-pairing-control",
    "reticulum-device-api-pairing",
    "reticulum-device-api-handoff",
    "reticulum-device-api-pairing-policy",
    "reticulum-device-api-session",
    "reticulum-node-core",
    "reticulum-radio-interface",
    "reticulum-radio-tx-dispatch",
    "reticulum-rns-inbox-store",
    "reticulum-rns-rete",
    "reticulum-semantic-roundtrip-hil",
    "reticulum-storage-actor",
    "reticulum-submission-projector",
    "reticulum-tx-dispatch",
    "reticulum-tx-handoff",
    "reticulum-tx-supervisor",
];

const TX_HIL_GRAPH_REQUIRED: [&str; 5] = [
    "reticulum-board-heltec-tracker-v2-radio",
    "reticulum-board-heltec-tracker-v2-tx-hil",
    "reticulum-radio-interface",
    "reticulum-radio-lora-phy",
    "reticulum-semantic-roundtrip-hil",
];

const TX_HIL_GRAPH_FORBIDDEN: [&str; 27] = [
    "leviculum-core",
    "lxmf-rs",
    "rete-core",
    "rete-lxmf",
    "rete-stack",
    "rete-transport",
    "reticulum-device-api-adapter",
    "reticulum-device-api-credential-store",
    "reticulum-device-api-credentials",
    "reticulum-device-api-framing",
    "reticulum-device-api-pairing-control",
    "reticulum-device-api-pairing",
    "reticulum-device-api-handoff",
    "reticulum-device-api-pairing-policy",
    "reticulum-device-api-session",
    "reticulum-board-heltec-vision-master-e290-radio",
    "reticulum-node-core",
    "reticulum-radio-tx-dispatch",
    "reticulum-rns-inbox-store",
    "reticulum-rns-rete",
    "reticulum-storage-actor",
    "reticulum-storage-journal",
    "reticulum-storage-model",
    "reticulum-submission-projector",
    "reticulum-tx-dispatch",
    "reticulum-tx-handoff",
    "reticulum-tx-supervisor",
];

fn validate_tx_hil_graph_boundary(tree: &str) -> Result<(), String> {
    for required in TX_HIL_GRAPH_REQUIRED {
        if !cargo_tree_contains_package(tree, required) {
            return Err(format!(
                "hazardous TX HIL graph is missing required {required}"
            ));
        }
    }
    for forbidden in TX_HIL_GRAPH_FORBIDDEN {
        if cargo_tree_contains_package(tree, forbidden) {
            return Err(format!(
                "hazardous TX HIL graph contains forbidden {forbidden}"
            ));
        }
    }
    Ok(())
}

const SEMANTIC_TX_HIL_GRAPH_REQUIRED: [&str; 9] = [
    "reticulum-board-heltec-tracker-v2-radio",
    "reticulum-board-heltec-tracker-v2-tx-hil",
    "reticulum-radio-interface",
    "reticulum-radio-lora-phy",
    "reticulum-rns-rete",
    "reticulum-semantic-roundtrip-hil",
    "rete-core",
    "rete-stack",
    "rete-transport",
];

const SEMANTIC_TX_HIL_GRAPH_FORBIDDEN: [&str; 23] = [
    "leviculum-core",
    "lxmf-rs",
    "rete-lxmf",
    "reticulum-device-api-adapter",
    "reticulum-device-api-credential-store",
    "reticulum-device-api-credentials",
    "reticulum-device-api-framing",
    "reticulum-device-api-pairing-control",
    "reticulum-device-api-pairing",
    "reticulum-device-api-handoff",
    "reticulum-device-api-pairing-policy",
    "reticulum-device-api-session",
    "reticulum-board-heltec-vision-master-e290-radio",
    "reticulum-node-core",
    "reticulum-radio-tx-dispatch",
    "reticulum-rns-inbox-store",
    "reticulum-storage-actor",
    "reticulum-storage-journal",
    "reticulum-storage-model",
    "reticulum-submission-projector",
    "reticulum-tx-dispatch",
    "reticulum-tx-handoff",
    "reticulum-tx-supervisor",
];

fn validate_semantic_tx_hil_graph_boundary(tree: &str) -> Result<(), String> {
    for required in SEMANTIC_TX_HIL_GRAPH_REQUIRED {
        if !cargo_tree_contains_package(tree, required) {
            return Err(format!(
                "semantic announce TX HIL graph is missing required {required}"
            ));
        }
    }
    for forbidden in SEMANTIC_TX_HIL_GRAPH_FORBIDDEN {
        if cargo_tree_contains_package(tree, forbidden) {
            return Err(format!(
                "semantic announce TX HIL graph contains forbidden {forbidden}"
            ));
        }
    }
    if !tree.lines().any(|line| {
        line.contains("reticulum-heltec-tracker-v2-tx-hil")
            && line.contains("features=[semantic-announce-hil,tracker-radio]")
    }) {
        return Err(
            "semantic announce TX HIL graph did not activate only its explicit root feature"
                .to_owned(),
        );
    }
    if !tree
        .lines()
        .any(|line| line.contains("reticulum-rns-rete") && line.contains("features=[conformance]"))
    {
        return Err(
            "semantic announce TX HIL graph did not activate the Rete adapter conformance surface"
                .to_owned(),
        );
    }
    if !tree.lines().any(|line| {
        line.contains("reticulum-semantic-roundtrip-hil")
            && line.ends_with("features=[semantic-announce-hil]")
    }) {
        return Err(
            "semantic announce TX HIL graph did not select the exact portable fixture feature"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_semantic_roundtrip_tx_hil_graph_boundary(tree: &str) -> Result<(), String> {
    for required in SEMANTIC_TX_HIL_GRAPH_REQUIRED {
        if !cargo_tree_contains_package(tree, required) {
            return Err(format!(
                "semantic round-trip TX HIL graph is missing required {required}"
            ));
        }
    }
    for forbidden in SEMANTIC_TX_HIL_GRAPH_FORBIDDEN {
        if cargo_tree_contains_package(tree, forbidden) {
            return Err(format!(
                "semantic round-trip TX HIL graph contains forbidden {forbidden}"
            ));
        }
    }
    if !tree.lines().any(|line| {
        line.contains("reticulum-heltec-tracker-v2-tx-hil")
            && line.contains("features=[semantic-roundtrip-hil,tracker-radio]")
    }) {
        return Err(
            "semantic round-trip TX HIL graph did not activate only its explicit root feature"
                .to_owned(),
        );
    }
    if !tree
        .lines()
        .any(|line| line.contains("reticulum-rns-rete") && line.contains("features=[]"))
    {
        return Err(
            "semantic round-trip TX HIL graph did not keep the Rete adapter on its product surface"
                .to_owned(),
        );
    }
    if tree
        .lines()
        .any(|line| line.contains("reticulum-rns-rete") && line.contains("conformance"))
    {
        return Err(
            "semantic round-trip TX HIL graph unexpectedly enabled Rete conformance helpers"
                .to_owned(),
        );
    }
    if !tree.contains("static_cell") {
        return Err("semantic round-trip TX HIL graph is missing static_cell".to_owned());
    }
    if !tree.lines().any(|line| {
        line.contains("reticulum-semantic-roundtrip-hil")
            && line.ends_with("features=[semantic-roundtrip-hil]")
    }) {
        return Err(
            "semantic round-trip TX HIL graph did not select the exact portable fixture feature"
                .to_owned(),
        );
    }
    Ok(())
}

const E290_SEMANTIC_HIL_GRAPH_REQUIRED: [&str; 9] = [
    "reticulum-board-heltec-vision-master-e290",
    "reticulum-board-heltec-vision-master-e290-radio",
    "reticulum-semantic-roundtrip-hil",
    "reticulum-radio-interface",
    "reticulum-radio-lora-phy",
    "reticulum-rns-rete",
    "rete-core",
    "rete-stack",
    "rete-transport",
];

const E290_SEMANTIC_HIL_GRAPH_FORBIDDEN: [&str; 26] = [
    "leviculum-core",
    "lxmf-rs",
    "rete-lxmf",
    "reticulum-board-heltec-tracker-v2-radio",
    "reticulum-board-heltec-tracker-v2-tx-hil",
    "reticulum-board-heltec-tracker-v2",
    "reticulum-heltec-tracker-v2-tx-hil",
    "reticulum-device-api-adapter",
    "reticulum-device-api-credential-store",
    "reticulum-device-api-credentials",
    "reticulum-device-api-framing",
    "reticulum-device-api-pairing-control",
    "reticulum-device-api-pairing",
    "reticulum-device-api-handoff",
    "reticulum-device-api-pairing-policy",
    "reticulum-device-api-session",
    "reticulum-interface-router",
    "reticulum-node-core",
    "reticulum-radio-tx-dispatch",
    "reticulum-rns-inbox-store",
    "reticulum-storage-actor",
    "reticulum-storage-journal",
    "reticulum-submission-projector",
    "reticulum-tx-dispatch",
    "reticulum-tx-handoff",
    "reticulum-tx-supervisor",
];

fn validate_e290_semantic_hil_graph_boundary(tree: &str) -> Result<(), String> {
    for required in E290_SEMANTIC_HIL_GRAPH_REQUIRED {
        if !cargo_tree_contains_package(tree, required) {
            return Err(format!(
                "E290 semantic HIL graph is missing required {required}"
            ));
        }
    }
    for forbidden in E290_SEMANTIC_HIL_GRAPH_FORBIDDEN {
        if cargo_tree_contains_package(tree, forbidden) {
            return Err(format!(
                "E290 semantic HIL graph contains forbidden {forbidden}"
            ));
        }
    }

    let policy_line = tree
        .lines()
        .find(|line| line.contains("reticulum-semantic-roundtrip-hil"))
        .ok_or_else(|| "E290 semantic HIL graph has no shared policy package".to_owned())?;
    if !policy_line.ends_with("features=[semantic-roundtrip-hil]") {
        return Err(format!(
            "E290 semantic HIL must reach only the board-independent semantic round-trip fixture feature, observed {policy_line}"
        ));
    }
    Ok(())
}

const E290_INBOX_COMMIT_FAULT_HIL_FEATURE: &str = "rns-inbox-commit-fault-hil";
const E290_RUNTIME_MEASUREMENT_HIL_FEATURE: &str = "runtime-measurement-hil";

fn validate_e290_inbox_commit_fault_hil_workspace(workspace: &Path) -> Result<(), String> {
    let product = workspace.join("firmware/heltec-vision-master-e290-node");
    let library = fs::read_to_string(product.join("src/lib.rs"))
        .map_err(|error| format!("could not read E290 library source: {error}"))?;
    let storage = fs::read_to_string(product.join("src/platform_storage.rs"))
        .map_err(|error| format!("could not read E290 storage-owner source: {error}"))?;
    let build = fs::read_to_string(product.join("build.rs"))
        .map_err(|error| format!("could not read E290 build policy: {error}"))?;
    let fixture = fs::read_to_string(product.join("src/inbox_admission_fault_hil.rs"))
        .map_err(|error| format!("could not read E290 commit-fault fixture: {error}"))?;
    validate_e290_inbox_commit_fault_hil_sources(&library, &storage, &build, &fixture)
}

fn validate_e290_inbox_commit_fault_hil_sources(
    library: &str,
    storage: &str,
    build: &str,
    fixture: &str,
) -> Result<(), String> {
    let module_declaration = format!(
        "#[cfg(all(\n    feature = \"{E290_INBOX_COMMIT_FAULT_HIL_FEATURE}\",\n    any(test, target_arch = \"xtensa\")\n))]\npub mod inbox_admission_fault_hil;"
    );
    if library.matches(&module_declaration).count() != 1 {
        return Err(
            "the commit-fault module must have one exact feature-and-test-or-Xtensa-gated library declaration"
                .to_owned(),
        );
    }

    let constructor = "SuppressThirdWrite::new(region)";
    let observation = "observe_product_quarantine(";
    if storage.matches(constructor).count() != 1 || storage.matches(observation).count() != 1 {
        return Err(
            "the storage owner must contain one wrapper construction and one quarantine observation"
                .to_owned(),
        );
    }
    let offer_start = storage
        .find("pub(crate) fn offer_inbound(")
        .ok_or_else(|| "the storage owner has no inbound-offer method".to_owned())?;
    let offer_tail = &storage[offer_start..];
    let offer_end = offer_tail
        .find("/// Count one input discarded")
        .ok_or_else(|| "the inbound-offer method has no stable end boundary".to_owned())?;
    let offer = &offer_tail[..offer_end];
    let wrapper_position = offer
        .find(constructor)
        .ok_or_else(|| "the commit-fault wrapper is not scoped to inbound offer".to_owned())?;
    let disable_position = offer
        .find("self.inbox_service_enabled = false;")
        .ok_or_else(|| "the inbound fault path does not disable inbox service".to_owned())?;
    let drop_position = offer[disable_position..]
        .find("self.record_inbound_drop();")
        .map(|offset| disable_position + offset)
        .ok_or_else(|| "the inbound fault path does not record its dropped candidate".to_owned())?;
    let observation_position = offer
        .find(observation)
        .ok_or_else(|| "the quarantine observation is not scoped to inbound offer".to_owned())?;
    if !(disable_position < drop_position && drop_position < observation_position) {
        return Err(
            "quarantine evidence must be published after service disablement and drop accounting"
                .to_owned(),
        );
    }
    for (position, label) in [
        (wrapper_position, "wrapper construction"),
        (observation_position, "quarantine observation"),
    ] {
        if !immediately_preceded_by_feature_cfg(
            offer,
            position,
            E290_INBOX_COMMIT_FAULT_HIL_FEATURE,
        ) {
            return Err(format!("the {label} is not directly feature-gated"));
        }
    }

    for required in [
        "CARGO_FEATURE_JOURNAL_SCHEMA2_DEV_REPROVISION",
        "CARGO_FEATURE_RNS_INBOX_COMMIT_FAULT_HIL",
        "CARGO_FEATURE_RUNTIME_MEASUREMENT_HIL",
        "journal-schema2-dev-reprovision, rns-inbox-commit-fault-hil, and runtime-measurement-hil are mutually exclusive",
    ] {
        if !build.contains(required) {
            return Err(format!("the E290 build policy is missing {required:?}"));
        }
    }
    for required in [
        "pub struct SuppressThirdWrite",
        "RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE",
        "#[used]",
    ] {
        if !fixture.contains(required) {
            return Err(format!("the commit-fault fixture is missing {required:?}"));
        }
    }
    if fixture.contains("no_mangle") {
        return Err(
            "the safe library fixture must retain its debugger symbol without no_mangle".to_owned(),
        );
    }
    Ok(())
}

const E290_RUNTIME_MEASUREMENT_EVIDENCE: &str = "RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE";
const E290_RUNTIME_PROOF_TRACE_EVIDENCE: &str = "RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE";
const E290_RUNTIME_MEASUREMENT_STACK_MARKER: &str = "RETICULUM_RUNTIME_MEASUREMENT_STACK_MARKER";
const E290_RUNTIME_MEASUREMENT_HOOKS: [&str; 2] = ["_esp_alloc_alloc", "_esp_alloc_dealloc"];

fn validate_e290_runtime_measurement_hil_workspace(workspace: &Path) -> Result<(), String> {
    let product = workspace.join("firmware/heltec-vision-master-e290-node");
    let mut sources = Vec::new();
    collect_workspace_rust_sources(workspace, &product.join("src"), &mut sources)?;
    let build = fs::read_to_string(product.join("build.rs"))
        .map_err(|error| format!("could not read E290 build policy: {error}"))?;
    validate_e290_runtime_measurement_hil_sources(&sources, &build)
}

fn validate_e290_runtime_measurement_hil_sources(
    sources: &[(String, String)],
    build: &str,
) -> Result<(), String> {
    const PRODUCT: &str = "firmware/heltec-vision-master-e290-node/src/";
    const LIBRARY: &str = "firmware/heltec-vision-master-e290-node/src/lib.rs";
    const MAIN: &str = "firmware/heltec-vision-master-e290-node/src/main.rs";
    const SAFE_MODULE: &str = "firmware/heltec-vision-master-e290-node/src/runtime_measurement.rs";
    const STACK_MODULE: &str =
        "firmware/heltec-vision-master-e290-node/src/runtime_measurement_stack_hil.rs";

    let exact_source = |path: &str| -> Result<&str, String> {
        let matching = sources
            .iter()
            .filter(|(candidate, _)| candidate == path)
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(format!(
                "runtime-measurement source policy expected exactly one {path}, found {}",
                matching.len()
            ));
        }
        Ok(matching[0].1.as_str())
    };

    let library = exact_source(LIBRARY)?;
    let main = exact_source(MAIN)?;
    let runtime = exact_source(SAFE_MODULE)?;
    let stack = exact_source(STACK_MODULE)?;

    let library_declaration = format!(
        "#[cfg(feature = \"{E290_RUNTIME_MEASUREMENT_HIL_FEATURE}\")]\npub mod runtime_measurement;"
    );
    if library.matches(&library_declaration).count() != 1
        || library.matches("runtime_measurement").count() != 1
    {
        return Err(
            "the safe runtime-measurement module must have one exact feature-gated library declaration"
                .to_owned(),
        );
    }

    let stack_declaration = format!(
        "#[cfg(feature = \"{E290_RUNTIME_MEASUREMENT_HIL_FEATURE}\")]\nmod runtime_measurement_stack_hil;"
    );
    if main.matches(&stack_declaration).count() != 1 {
        return Err(
            "the target stack-watermark module must have one exact runtime-measurement feature gate"
                .to_owned(),
        );
    }
    let stack_call = "runtime_measurement_stack_hil::";
    let stack_call_sites = main
        .lines()
        .filter(|line| {
            line.find(stack_call)
                .is_some_and(|position| line[position + stack_call.len()..].contains('('))
        })
        .count();
    if stack_call_sites != 1 {
        return Err(
            "the target main must contain exactly one runtime-measurement stack initialization site"
                .to_owned(),
        );
    }

    let evidence_definition = format!("pub static {E290_RUNTIME_MEASUREMENT_EVIDENCE}");
    if runtime.matches(&evidence_definition).count() != 1 {
        return Err(
            "the safe runtime-measurement module must own one public debugger evidence static"
                .to_owned(),
        );
    }
    let evidence_position = runtime
        .find(&evidence_definition)
        .expect("the evidence definition count was checked");
    if !immediately_preceded_by_exact_attribute(runtime, evidence_position, "#[used]") {
        return Err(
            "the runtime-measurement evidence static must be retained with #[used]".to_owned(),
        );
    }
    let proof_trace_definition = format!("pub static {E290_RUNTIME_PROOF_TRACE_EVIDENCE}");
    if runtime.matches(&proof_trace_definition).count() != 1 {
        return Err(
            "the safe runtime-measurement module must own one public proof-trace evidence static"
                .to_owned(),
        );
    }
    let proof_trace_position = runtime
        .find(&proof_trace_definition)
        .expect("the proof-trace evidence definition count was checked");
    if !immediately_preceded_by_exact_attribute(runtime, proof_trace_position, "#[used]") {
        return Err("the proof-trace evidence static must be retained with #[used]".to_owned());
    }
    if runtime.contains("no_mangle") {
        return Err(
            "the safe runtime-measurement evidence must retain its identifier without no_mangle"
                .to_owned(),
        );
    }

    let marker_definition = format!("static mut {E290_RUNTIME_MEASUREMENT_STACK_MARKER}");
    if stack.matches(&marker_definition).count() != 1
        || !stack.contains("#[used]")
        || !stack.contains("#[unsafe(no_mangle)]")
        || !stack.contains("__zero_bss")
    {
        return Err(
            "the target stack module must own one retained startup marker and reset-time stack hook"
                .to_owned(),
        );
    }
    for required in [
        "let innermost_sp = current_stack_pointer();",
        "if address >= innermost_sp {",
        "!stack_watermark_word(address)",
    ] {
        if stack.matches(required).count() != 1 {
            return Err(format!(
                "the target stack scanner must retain one per-read live-frame guard containing {required:?}"
            ));
        }
    }

    for required in [
        "CARGO_FEATURE_RUNTIME_MEASUREMENT_HIL",
        "journal-schema2-dev-reprovision, rns-inbox-commit-fault-hil, and runtime-measurement-hil are mutually exclusive",
    ] {
        if !build.contains(required) {
            return Err(format!("the E290 build policy is missing {required:?}"));
        }
    }

    let tracked_identifiers = [
        "runtime_measurement",
        E290_RUNTIME_MEASUREMENT_EVIDENCE,
        E290_RUNTIME_PROOF_TRACE_EVIDENCE,
        E290_RUNTIME_MEASUREMENT_STACK_MARKER,
        E290_RUNTIME_MEASUREMENT_HOOKS[0],
        E290_RUNTIME_MEASUREMENT_HOOKS[1],
    ];
    for (path, source) in sources {
        if !path.starts_with(PRODUCT) {
            return Err(format!(
                "runtime-measurement source inventory escaped the E290 product: {path}"
            ));
        }
        if path != LIBRARY && path != SAFE_MODULE && path != STACK_MODULE {
            validate_runtime_measurement_consumer_source(path, source, &tracked_identifiers)?;
        }
    }

    for (identifier, expected_path, definition_fragment) in [
        (
            E290_RUNTIME_MEASUREMENT_EVIDENCE,
            SAFE_MODULE,
            "static RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE",
        ),
        (
            E290_RUNTIME_PROOF_TRACE_EVIDENCE,
            SAFE_MODULE,
            "static RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE",
        ),
        (
            E290_RUNTIME_MEASUREMENT_STACK_MARKER,
            STACK_MODULE,
            "static mut RETICULUM_RUNTIME_MEASUREMENT_STACK_MARKER",
        ),
    ] {
        let definitions = sources
            .iter()
            .filter(|(_, source)| source.contains(definition_fragment))
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>();
        if definitions != [expected_path] {
            return Err(format!(
                "{identifier} must be defined only in {expected_path}, observed {definitions:?}"
            ));
        }
    }
    for hook in E290_RUNTIME_MEASUREMENT_HOOKS {
        let definition = format!("fn {hook}");
        let definitions = sources
            .iter()
            .filter(|(_, source)| source.contains(&definition))
            .map(|(path, _)| path.as_str())
            .collect::<Vec<_>>();
        if definitions.len() != 1 {
            return Err(format!(
                "runtime-measurement HIL must define exactly one {hook} allocator callback, observed {definitions:?}"
            ));
        }
    }
    let allocator_hook = concat!(
        "#[cfg(feature = \"runtime-measurement-hil\")]\n",
        "#[unsafe(no_mangle)]\n",
        "fn _esp_alloc_alloc(\n",
        "    heap: &::esp_alloc::EspHeap,\n",
        "    _capabilities: ::esp_alloc::export::enumset::EnumSet<::esp_alloc::MemoryCapability>,\n",
        "    pointer: usize,\n",
        "    _size: usize,\n",
        ") {",
    );
    let deallocator_hook = concat!(
        "#[cfg(feature = \"runtime-measurement-hil\")]\n",
        "#[unsafe(no_mangle)]\n",
        "fn _esp_alloc_dealloc(_heap: &::esp_alloc::EspHeap, _pointer: usize, _size: usize) {",
    );
    if main.matches(allocator_hook).count() != 1 || main.matches(deallocator_hook).count() != 1 {
        return Err(
            "the runtime-measurement allocator callbacks must retain their exact pinned esp-alloc 0.10 Rust ABI, attributes, parameter order, and unit return"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_runtime_measurement_consumer_source(
    path: &str,
    source: &str,
    tracked_identifiers: &[&str],
) -> Result<(), String> {
    let file = syn::parse_file(source)
        .map_err(|error| format!("could not parse runtime-measurement consumer {path}: {error}"))?;
    for item in &file.items {
        let rendered = item.to_token_stream().to_string();
        if !contains_any_identifier(&rendered, tracked_identifiers) {
            continue;
        }
        if tokens_start_with_feature_cfg(&rendered, E290_RUNTIME_MEASUREMENT_HIL_FEATURE) {
            continue;
        }
        if let Item::Fn(function) = item {
            let signature = function.sig.to_token_stream().to_string();
            if contains_any_identifier(&signature, tracked_identifiers) {
                return Err(format!(
                    "runtime-measurement identifier in {path} function signature is not directly feature-gated"
                ));
            }
            validate_runtime_measurement_consumer_block(
                path,
                &function.block,
                tracked_identifiers,
            )?;
            continue;
        }
        return Err(format!(
            "runtime-measurement identifier in {path} is not directly feature-gated"
        ));
    }
    Ok(())
}

fn validate_runtime_measurement_consumer_block(
    path: &str,
    block: &syn::Block,
    tracked_identifiers: &[&str],
) -> Result<(), String> {
    for statement in &block.stmts {
        let rendered = statement.to_token_stream().to_string();
        if !contains_any_identifier(&rendered, tracked_identifiers) {
            continue;
        }
        if tokens_start_with_feature_cfg(&rendered, E290_RUNTIME_MEASUREMENT_HIL_FEATURE) {
            continue;
        }
        match statement {
            syn::Stmt::Item(Item::Fn(function)) => validate_runtime_measurement_consumer_block(
                path,
                &function.block,
                tracked_identifiers,
            )?,
            syn::Stmt::Local(local) => {
                let pattern = local.pat.to_token_stream().to_string();
                if contains_any_identifier(&pattern, tracked_identifiers) {
                    return Err(format!(
                        "runtime-measurement local binding in {path} is not directly feature-gated"
                    ));
                }
                let initializer = local.init.as_ref().ok_or_else(|| {
                    format!(
                        "runtime-measurement local statement in {path} has no feature-gated initializer"
                    )
                })?;
                validate_runtime_measurement_consumer_expression(
                    path,
                    &initializer.expr,
                    tracked_identifiers,
                )?;
                if let Some((_, diverge)) = &initializer.diverge {
                    validate_runtime_measurement_consumer_expression(
                        path,
                        diverge,
                        tracked_identifiers,
                    )?;
                }
            }
            syn::Stmt::Expr(expression, _) => validate_runtime_measurement_consumer_expression(
                path,
                expression,
                tracked_identifiers,
            )?,
            _ => {
                return Err(format!(
                    "runtime-measurement statement in {path} is not directly feature-gated"
                ));
            }
        }
    }
    Ok(())
}

fn validate_runtime_measurement_consumer_expression(
    path: &str,
    expression: &syn::Expr,
    tracked_identifiers: &[&str],
) -> Result<(), String> {
    let rendered = expression.to_token_stream().to_string();
    if !contains_any_identifier(&rendered, tracked_identifiers)
        || tokens_start_with_feature_cfg(&rendered, E290_RUNTIME_MEASUREMENT_HIL_FEATURE)
    {
        return Ok(());
    }

    match expression {
        syn::Expr::Async(expression) => validate_runtime_measurement_consumer_block(
            path,
            &expression.block,
            tracked_identifiers,
        ),
        syn::Expr::Block(expression) => validate_runtime_measurement_consumer_block(
            path,
            &expression.block,
            tracked_identifiers,
        ),
        syn::Expr::Const(expression) => validate_runtime_measurement_consumer_block(
            path,
            &expression.block,
            tracked_identifiers,
        ),
        syn::Expr::ForLoop(expression) => {
            if contains_any_identifier(
                &expression.expr.to_token_stream().to_string(),
                tracked_identifiers,
            ) {
                return Err(format!(
                    "runtime-measurement loop input in {path} is not directly feature-gated"
                ));
            }
            validate_runtime_measurement_consumer_block(path, &expression.body, tracked_identifiers)
        }
        syn::Expr::If(expression) => {
            if contains_any_identifier(
                &expression.cond.to_token_stream().to_string(),
                tracked_identifiers,
            ) {
                return Err(format!(
                    "runtime-measurement condition in {path} is not directly feature-gated"
                ));
            }
            validate_runtime_measurement_consumer_block(
                path,
                &expression.then_branch,
                tracked_identifiers,
            )?;
            if let Some((_, otherwise)) = &expression.else_branch {
                validate_runtime_measurement_consumer_expression(
                    path,
                    otherwise,
                    tracked_identifiers,
                )?;
            }
            Ok(())
        }
        syn::Expr::Loop(expression) => {
            validate_runtime_measurement_consumer_block(path, &expression.body, tracked_identifiers)
        }
        syn::Expr::Match(expression) => {
            if contains_any_identifier(
                &expression.expr.to_token_stream().to_string(),
                tracked_identifiers,
            ) {
                return Err(format!(
                    "runtime-measurement match input in {path} is not directly feature-gated"
                ));
            }
            for arm in &expression.arms {
                let rendered = arm.to_token_stream().to_string();
                if !contains_any_identifier(&rendered, tracked_identifiers)
                    || tokens_start_with_feature_cfg(
                        &rendered,
                        E290_RUNTIME_MEASUREMENT_HIL_FEATURE,
                    )
                {
                    continue;
                }
                validate_runtime_measurement_consumer_expression(
                    path,
                    &arm.body,
                    tracked_identifiers,
                )?;
            }
            Ok(())
        }
        syn::Expr::TryBlock(expression) => validate_runtime_measurement_consumer_block(
            path,
            &expression.block,
            tracked_identifiers,
        ),
        syn::Expr::Unsafe(expression) => validate_runtime_measurement_consumer_block(
            path,
            &expression.block,
            tracked_identifiers,
        ),
        syn::Expr::While(expression) => {
            if contains_any_identifier(
                &expression.cond.to_token_stream().to_string(),
                tracked_identifiers,
            ) {
                return Err(format!(
                    "runtime-measurement loop condition in {path} is not directly feature-gated"
                ));
            }
            validate_runtime_measurement_consumer_block(path, &expression.body, tracked_identifiers)
        }
        _ => Err(format!(
            "runtime-measurement expression in {path} is not directly feature-gated"
        )),
    }
}

fn contains_any_identifier(rendered: &str, identifiers: &[&str]) -> bool {
    identifiers
        .iter()
        .any(|identifier| rendered.contains(identifier))
}

fn tokens_start_with_feature_cfg(rendered: &str, feature: &str) -> bool {
    rendered.starts_with(&format!("# [cfg (feature = \"{feature}\")]"))
}

fn immediately_preceded_by_exact_attribute(source: &str, position: usize, attribute: &str) -> bool {
    let statement_line = source[..position].rfind('\n').map_or(0, |index| index + 1);
    source[..statement_line]
        .lines()
        .next_back()
        .is_some_and(|line| line.trim() == attribute)
}

fn immediately_preceded_by_feature_cfg(source: &str, position: usize, feature: &str) -> bool {
    let statement_line = source[..position].rfind('\n').map_or(0, |index| index + 1);
    source[..statement_line]
        .lines()
        .next_back()
        .is_some_and(|line| line.trim() == format!("#[cfg(feature = \"{feature}\")]"))
}

const E290_NODE_GRAPH_REQUIRED: [&str; 38] = [
    "embedded-storage",
    "esp-alloc",
    "esp-storage",
    "reticulum-announce-clock",
    "reticulum-board-heltec-vision-master-e290",
    "reticulum-board-heltec-vision-master-e290-radio",
    "reticulum-device-api",
    "reticulum-device-api-adapter",
    "reticulum-device-api-credential-store",
    "reticulum-device-api-credentials",
    "reticulum-device-api-framing",
    "reticulum-device-api-handoff",
    "reticulum-device-api-pairing",
    "reticulum-device-api-pairing-control",
    "reticulum-device-api-pairing-policy",
    "reticulum-device-api-session",
    "reticulum-device-identity-store",
    "reticulum-interface-router",
    "reticulum-node-core",
    "reticulum-nor-flash-region",
    "reticulum-radio-interface",
    "reticulum-radio-lora-phy",
    "reticulum-radio-tx-dispatch",
    "reticulum-rns-inbox-store",
    "reticulum-rns-rete",
    "rete-core",
    "rete-stack",
    "rete-transport",
    "reticulum-storage-actor",
    "reticulum-storage-journal",
    "reticulum-storage-model",
    "reticulum-submission-projector",
    "reticulum-submission-runtime",
    "reticulum-tx-dispatch",
    "reticulum-tx-handoff",
    "reticulum-tx-supervisor",
    "esp-println",
    "static_cell",
];

const E290_NODE_GRAPH_FORBIDDEN: [&str; 11] = [
    "leviculum-core",
    "lxmf-rs",
    "rete-lxmf",
    "reticulum-board-heltec-tracker-v2",
    "reticulum-heltec-tracker-v2",
    "reticulum-heltec-vision-master-e290-qualification",
    "reticulum-heltec-vision-master-e290-semantic-hil",
    "reticulum-lab-rx-returned-fault-hil",
    "reticulum-rns-leviculum",
    "reticulum-rns-rete-rx",
    "reticulum-semantic-roundtrip-hil",
];

fn validate_e290_node_graph_boundary(tree: &str) -> Result<(), String> {
    validate_e290_node_graph_for_root_features(tree, "default", "permanent E290 node")
}

fn validate_e290_inbox_commit_fault_hil_graph_boundary(
    permanent: &str,
    hil: &str,
) -> Result<(), String> {
    validate_e290_node_graph_for_root_features(
        hil,
        E290_INBOX_COMMIT_FAULT_HIL_FEATURE,
        "E290 inbox commit-fault HIL",
    )?;
    let permanent_dependencies = permanent.lines().skip(1).collect::<Vec<_>>();
    let hil_dependencies = hil.lines().skip(1).collect::<Vec<_>>();
    if permanent_dependencies != hil_dependencies {
        return Err(
            "E290 inbox commit-fault HIL must change only the product root feature, not the dependency graph"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_e290_runtime_measurement_hil_graph_boundary(
    permanent: &str,
    hil: &str,
) -> Result<(), String> {
    validate_e290_node_graph_for_root_features(
        hil,
        E290_RUNTIME_MEASUREMENT_HIL_FEATURE,
        "E290 runtime-measurement HIL",
    )?;

    const DEFAULT_ESP_ALLOC_FEATURES: &str =
        "features=[compat,default,esp32s3,global-allocator,internal-heap-stats]";
    const HIL_ESP_ALLOC_FEATURES: &str =
        "features=[alloc-hooks,compat,default,esp32s3,global-allocator,internal-heap-stats]";
    let has_exact_features = |line: &&str, features: &str| {
        line.ends_with(features) || line.ends_with(&format!("{features} (*)"))
    };

    let permanent_esp_alloc = permanent
        .lines()
        .filter(|line| line.contains("esp-alloc "))
        .collect::<Vec<_>>();
    let hil_esp_alloc = hil
        .lines()
        .filter(|line| line.contains("esp-alloc "))
        .collect::<Vec<_>>();
    if permanent_esp_alloc.is_empty()
        || permanent_esp_alloc.len() != hil_esp_alloc.len()
        || permanent_esp_alloc
            .iter()
            .any(|line| !has_exact_features(line, DEFAULT_ESP_ALLOC_FEATURES))
        || hil_esp_alloc
            .iter()
            .any(|line| !has_exact_features(line, HIL_ESP_ALLOC_FEATURES))
    {
        return Err(
            "E290 runtime-measurement HIL must add only esp-alloc/alloc-hooks to every resolved esp-alloc edge"
                .to_owned(),
        );
    }

    let normalized = hil
        .replacen(
            &format!("features=[{E290_RUNTIME_MEASUREMENT_HIL_FEATURE}]"),
            "features=[default]",
            1,
        )
        .replace(HIL_ESP_ALLOC_FEATURES, DEFAULT_ESP_ALLOC_FEATURES);
    if permanent != normalized {
        return Err(
            "E290 runtime-measurement HIL may change only the product root feature and esp-alloc/alloc-hooks; package and all other dependency feature lines must match default"
                .to_owned(),
        );
    }
    Ok(())
}

fn validate_e290_node_graph_for_root_features(
    tree: &str,
    expected_root_features: &str,
    profile: &str,
) -> Result<(), String> {
    for required in E290_NODE_GRAPH_REQUIRED {
        if !cargo_tree_contains_package(tree, required) {
            return Err(format!("{profile} graph is missing required {required}"));
        }
    }
    for forbidden in E290_NODE_GRAPH_FORBIDDEN {
        if cargo_tree_contains_package(tree, forbidden) {
            return Err(format!(
                "{profile} graph contains deferred or foreign package {forbidden}"
            ));
        }
    }

    let root_line = tree
        .lines()
        .find(|line| line.starts_with("reticulum-heltec-vision-master-e290-node "))
        .ok_or_else(|| format!("{profile} graph has no product root"))?;
    let expected_root_suffix = format!("features=[{expected_root_features}]");
    if !root_line.ends_with(&expected_root_suffix) {
        return Err(format!(
            "{profile} must enable only {expected_root_features}, observed {root_line}"
        ));
    }
    for package in ["reticulum-device-api ", "reticulum-device-api-adapter "] {
        let line = tree
            .lines()
            .find(|line| line.contains(package))
            .ok_or_else(|| format!("{profile} graph has no {package}line"))?;
        if !line.ends_with("features=[experimental-rns-data,experimental-rns-inbox]") {
            return Err(format!(
                "{profile} must enable only target-safe experimental RNS DATA and durable inbox operations on {package}, observed {line}"
            ));
        }
    }
    for package in [
        "reticulum-rns-inbox-store ",
        "reticulum-device-api-credential-store ",
        "reticulum-device-api-credentials ",
        "reticulum-device-api-framing ",
        "reticulum-device-api-handoff ",
        "reticulum-device-api-pairing ",
        "reticulum-device-api-pairing-control ",
        "reticulum-device-api-pairing-policy ",
        "reticulum-device-api-session ",
    ] {
        let line = tree
            .lines()
            .find(|line| line.contains(package))
            .ok_or_else(|| format!("{profile} graph has no {package}line"))?;
        if !line.ends_with("features=[]") {
            return Err(format!(
                "{profile} must keep the durable inbox, credential, authentication, and pre-authentication control packages feature-free; observed {line}"
            ));
        }
    }
    let println_line = tree
        .lines()
        .find(|line| line.contains("esp-println "))
        .ok_or_else(|| format!("{profile} graph has no esp-println line"))?;
    if !println_line.ends_with("features=[esp32s3,log-04,no-op]") {
        return Err(format!(
            "{profile} must reserve USB Serial/JTAG by enabling only the no-op esp-println backend, observed {println_line}"
        ));
    }
    Ok(())
}

fn validate_e290_node_feature_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let package_name = "reticulum-heltec-vision-master-e290-node";
    let package = exact_local_package(
        packages,
        workspace,
        package_name,
        "firmware/heltec-vision-master-e290-node/Cargo.toml",
    )?;
    let features = package["features"]
        .as_object()
        .ok_or_else(|| format!("{package_name} package has no feature map"))?;
    let expected_features = serde_json::json!({
        "default": [],
        "journal-schema2-dev-reprovision": [],
        "rns-inbox-commit-fault-hil": [],
        "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
    });
    if serde_json::Value::Object(features.clone()) != expected_features {
        return Err(format!(
            "{package_name} must expose only an empty default, two empty opt-in development features, and runtime-measurement-hil enabling only esp-alloc/alloc-hooks"
        ));
    }
    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-rns-inbox-store",
        &workspace.join("crates/rns-inbox-store"),
        false,
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-device-api-credential-store",
        &workspace.join("crates/device-api-credential-store"),
        false,
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-device-api-pairing-policy",
        &workspace.join("crates/device-api-pairing-policy"),
        false,
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-device-api-credentials",
        &workspace.join("crates/device-api-credentials"),
        false,
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-device-api-framing",
        &workspace.join("crates/device-api-framing"),
        false,
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-device-api-handoff",
        &workspace.join("crates/device-api-handoff"),
        false,
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-device-api-pairing",
        &workspace.join("crates/device-api-pairing"),
        false,
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-device-api-pairing-control",
        &workspace.join("crates/device-api-pairing-control"),
        false,
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-device-api-session",
        &workspace.join("crates/device-api-session"),
        false,
    )?;
    validate_exact_target_registry_dependency(
        dependencies,
        package_name,
        "embedded-storage",
        "=0.3.1",
        "cfg(target_arch = \"xtensa\")",
        false,
        &[],
    )?;
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "embedded-storage",
        "=0.3.1",
        Some("dev"),
        false,
        &[],
    )?;
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "rand_core",
        "=0.6.4",
        None,
        false,
        &[],
    )?;
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "zeroize",
        "=1.9.0",
        None,
        false,
        &[],
    )?;
    validate_exact_target_registry_dependency(
        dependencies,
        package_name,
        "esp-alloc",
        "=0.10.0",
        "cfg(target_arch = \"xtensa\")",
        true,
        &["esp32s3", "global-allocator", "internal-heap-stats"],
    )?;
    validate_exact_target_registry_dependency(
        dependencies,
        package_name,
        "esp-println",
        "=0.17.0",
        "cfg(target_arch = \"xtensa\")",
        false,
        &["esp32s3", "log-04", "no-op"],
    )?;
    Ok(())
}

fn validate_storage_hil_graph_boundary(tree: &str) -> Result<(), String> {
    for forbidden in STORAGE_HIL_GRAPH_FORBIDDEN {
        if cargo_tree_contains_package(tree, forbidden) {
            return Err(format!(
                "physical-storage HIL all-target graph contains forbidden {forbidden}"
            ));
        }
    }
    Ok(())
}

fn validate_portable_layer_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;

    for (package_name, relative_path, peer_name) in [
        (
            "reticulum-device-api",
            "crates/device-api",
            "reticulum-node-core",
        ),
        (
            "reticulum-node-core",
            "crates/node-core",
            "reticulum-device-api",
        ),
    ] {
        let expected_manifest = workspace.join(relative_path).join("Cargo.toml");
        let matching = packages
            .iter()
            .filter(|package| {
                package["name"].as_str() == Some(package_name)
                    && package["source"].is_null()
                    && package["manifest_path"].as_str().map(Path::new)
                        == Some(expected_manifest.as_path())
            })
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(format!(
                "expected exactly one local {package_name} package at {}, found {}",
                expected_manifest.display(),
                matching.len()
            ));
        }

        let dependencies = matching[0]["dependencies"]
            .as_array()
            .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
        for dependency in dependencies {
            let dependency_name = dependency["name"]
                .as_str()
                .ok_or_else(|| format!("{package_name} has a dependency without a name"))?;
            if dependency_name == peer_name {
                return Err(format!(
                    "{package_name} directly depends on peer portable layer {peer_name}"
                ));
            }
            if package_name == "reticulum-device-api"
                && is_rete_implementation_dependency(dependency_name)
            {
                return Err(format!(
                    "{package_name} directly depends on prohibited Rete implementation crate {dependency_name}"
                ));
            }
            if package_name == "reticulum-device-api"
                && matches!(
                    dependency_name,
                    "reticulum-radio-tx-dispatch"
                        | "reticulum-tx-handoff"
                        | "reticulum-tx-dispatch"
                        | "reticulum-tx-supervisor"
                )
            {
                return Err(format!(
                    "{package_name} directly depends on prohibited TX ownership crate {dependency_name}"
                ));
            }
            if is_platform_implementation_dependency(dependency, workspace) {
                return Err(format!(
                    "{package_name} directly depends on prohibited platform implementation crate {dependency_name}"
                ));
            }
        }
        if package_name == "reticulum-node-core" {
            if dependencies.len() != 3
                || dependencies
                    .iter()
                    .any(|dependency| !dependency["kind"].is_null())
            {
                return Err(format!(
                    "reticulum-node-core must have exactly three reviewed normal dependencies, found {}",
                    dependencies.len()
                ));
            }
            let rns_path = workspace.join("crates/rns-rete");
            let rns = dependencies
                .iter()
                .filter(|dependency| dependency["name"].as_str() == Some("reticulum-rns-rete"))
                .collect::<Vec<_>>();
            if rns.len() != 1
                || rns[0]["req"].as_str() != Some("*")
                || rns[0]["path"].as_str().map(Path::new) != Some(rns_path.as_path())
                || !rns[0]["source"].is_null()
                || rns[0]["optional"].as_bool() != Some(false)
                || !rns[0]["rename"].is_null()
                || !rns[0]["target"].is_null()
                || rns[0]["uses_default_features"].as_bool() != Some(false)
                || rns[0]["features"]
                    .as_array()
                    .is_none_or(|features| !features.is_empty())
            {
                return Err(
                    "reticulum-node-core must use one feature-free local reticulum-rns-rete dependency"
                        .to_owned(),
                );
            }
            for (name, requirement) in [("rand_core", "=0.6.4"), ("sha2", "=0.10.9")] {
                let dependency = dependencies
                    .iter()
                    .filter(|dependency| dependency["name"].as_str() == Some(name))
                    .collect::<Vec<_>>();
                if dependency.len() != 1
                    || dependency[0]["req"].as_str() != Some(requirement)
                    || dependency[0]["source"].as_str()
                        != Some("registry+https://github.com/rust-lang/crates.io-index")
                    || !dependency[0]["path"].is_null()
                    || dependency[0]["optional"].as_bool() != Some(false)
                    || !dependency[0]["rename"].is_null()
                    || !dependency[0]["target"].is_null()
                    || dependency[0]["uses_default_features"].as_bool() != Some(false)
                    || dependency[0]["features"]
                        .as_array()
                        .is_none_or(|features| !features.is_empty())
                {
                    return Err(format!(
                        "reticulum-node-core must use one feature-free registry {name} {requirement} dependency"
                    ));
                }
            }
        }
    }

    Ok(())
}

fn validate_device_api_edge_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;

    let framing_name = "reticulum-device-api-framing";
    let framing = exact_local_package(
        packages,
        workspace,
        framing_name,
        "crates/device-api-framing/Cargo.toml",
    )?;
    let framing_features = framing["features"]
        .as_object()
        .ok_or_else(|| format!("{framing_name} package has no feature map"))?;
    let framing_dependencies = framing["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{framing_name} package has no dependency array"))?;
    if !framing_features.is_empty()
        || framing_dependencies.len() != 1
        || framing_dependencies
            .iter()
            .any(|dependency| !dependency["kind"].is_null())
    {
        return Err(format!(
            "{framing_name} must expose no features and exactly one reviewed normal dependency"
        ));
    }
    validate_exact_registry_dependency(
        framing_dependencies,
        framing_name,
        "zeroize",
        "=1.9.0",
        None,
        false,
        &[],
    )?;

    let pairing_control_name = "reticulum-device-api-pairing-control";
    let pairing_control = exact_local_package(
        packages,
        workspace,
        pairing_control_name,
        "crates/device-api-pairing-control/Cargo.toml",
    )?;
    let pairing_control_features = pairing_control["features"]
        .as_object()
        .ok_or_else(|| format!("{pairing_control_name} package has no feature map"))?;
    let pairing_control_dependencies = pairing_control["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{pairing_control_name} package has no dependency array"))?;
    if !pairing_control_features.is_empty()
        || pairing_control_dependencies.len() != 1
        || pairing_control_dependencies
            .iter()
            .any(|dependency| !dependency["kind"].is_null())
    {
        return Err(format!(
            "{pairing_control_name} must expose no features and exactly one reviewed normal dependency"
        ));
    }
    validate_exact_local_dependency(
        pairing_control_dependencies,
        pairing_control_name,
        framing_name,
        &workspace.join("crates/device-api-framing"),
        false,
    )?;

    let handoff_name = "reticulum-device-api-handoff";
    let handoff = exact_local_package(
        packages,
        workspace,
        handoff_name,
        "crates/device-api-handoff/Cargo.toml",
    )?;
    let handoff_features = handoff["features"]
        .as_object()
        .ok_or_else(|| format!("{handoff_name} package has no feature map"))?;
    let handoff_dependencies = handoff["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{handoff_name} package has no dependency array"))?;
    if !handoff_features.is_empty() || handoff_dependencies.len() != 2 {
        return Err(format!(
            "{handoff_name} must expose no features and exactly two reviewed normal dependencies"
        ));
    }
    validate_exact_registry_dependency(
        handoff_dependencies,
        handoff_name,
        "embassy-sync",
        "=0.8.0",
        None,
        false,
        &[],
    )?;
    validate_exact_local_dependency(
        handoff_dependencies,
        handoff_name,
        "reticulum-device-api",
        &workspace.join("crates/device-api"),
        false,
    )?;

    let credentials_name = "reticulum-device-api-credentials";
    let credentials = exact_local_package(
        packages,
        workspace,
        credentials_name,
        "crates/device-api-credentials/Cargo.toml",
    )?;
    let credentials_features = credentials["features"]
        .as_object()
        .ok_or_else(|| format!("{credentials_name} package has no feature map"))?;
    let credentials_dependencies = credentials["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{credentials_name} package has no dependency array"))?;
    if !credentials_features.is_empty()
        || credentials_dependencies.len() != 3
        || credentials_dependencies
            .iter()
            .any(|dependency| !dependency["kind"].is_null())
    {
        return Err(format!(
            "{credentials_name} must expose no features and exactly three reviewed normal dependencies"
        ));
    }
    validate_exact_local_dependency(
        credentials_dependencies,
        credentials_name,
        "reticulum-device-api",
        &workspace.join("crates/device-api"),
        false,
    )?;
    for (dependency_name, requirement) in [("subtle", "=2.6.1"), ("zeroize", "=1.9.0")] {
        validate_exact_registry_dependency(
            credentials_dependencies,
            credentials_name,
            dependency_name,
            requirement,
            None,
            false,
            &[],
        )?;
    }

    let pairing_name = "reticulum-device-api-pairing-policy";
    let pairing = exact_local_package(
        packages,
        workspace,
        pairing_name,
        "crates/device-api-pairing-policy/Cargo.toml",
    )?;
    let pairing_features = pairing["features"]
        .as_object()
        .ok_or_else(|| format!("{pairing_name} package has no feature map"))?;
    if pairing_features.len() != 1
        || pairing_features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| !values.is_empty())
    {
        return Err(format!(
            "{pairing_name} must expose only one empty default feature"
        ));
    }
    let pairing_dependencies = pairing["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{pairing_name} package has no dependency array"))?;
    if pairing_dependencies.len() != 1
        || pairing_dependencies
            .iter()
            .any(|dependency| !dependency["kind"].is_null())
    {
        return Err(format!(
            "{pairing_name} must have exactly one reviewed normal dependency"
        ));
    }
    validate_exact_local_dependency(
        pairing_dependencies,
        pairing_name,
        "reticulum-device-api-credentials",
        &workspace.join("crates/device-api-credentials"),
        false,
    )?;

    let pairing_protocol_name = "reticulum-device-api-pairing";
    let pairing_protocol = exact_local_package(
        packages,
        workspace,
        pairing_protocol_name,
        "crates/device-api-pairing/Cargo.toml",
    )?;
    let pairing_protocol_features = pairing_protocol["features"]
        .as_object()
        .ok_or_else(|| format!("{pairing_protocol_name} package has no feature map"))?;
    let pairing_protocol_dependencies = pairing_protocol["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{pairing_protocol_name} package has no dependency array"))?;
    let pairing_protocol_normal_dependencies = pairing_protocol_dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .count();
    let pairing_protocol_dev_dependencies = pairing_protocol_dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .count();
    if !pairing_protocol_features.is_empty()
        || pairing_protocol_dependencies.len() != 6
        || pairing_protocol_normal_dependencies != 5
        || pairing_protocol_dev_dependencies != 1
    {
        return Err(format!(
            "{pairing_protocol_name} must expose no features and exactly five reviewed normal dependencies plus one test-only dependency"
        ));
    }
    for (dependency_name, requirement) in [
        ("hmac", "=0.12.1"),
        ("sha2", "=0.10.9"),
        ("zeroize", "=1.9.0"),
    ] {
        validate_exact_registry_dependency(
            pairing_protocol_dependencies,
            pairing_protocol_name,
            dependency_name,
            requirement,
            None,
            false,
            &[],
        )?;
    }
    for (dependency_name, relative_path) in [
        (
            "reticulum-device-api-credentials",
            "crates/device-api-credentials",
        ),
        ("reticulum-device-api-framing", "crates/device-api-framing"),
    ] {
        validate_exact_local_dependency(
            pairing_protocol_dependencies,
            pairing_protocol_name,
            dependency_name,
            &workspace.join(relative_path),
            false,
        )?;
    }
    validate_exact_registry_dependency(
        pairing_protocol_dependencies,
        pairing_protocol_name,
        "hex",
        "=0.4.3",
        Some("dev"),
        true,
        &[],
    )?;

    let session_name = "reticulum-device-api-session";
    let session = exact_local_package(
        packages,
        workspace,
        session_name,
        "crates/device-api-session/Cargo.toml",
    )?;
    let session_features = session["features"]
        .as_object()
        .ok_or_else(|| format!("{session_name} package has no feature map"))?;
    let session_dependencies = session["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{session_name} package has no dependency array"))?;
    let session_normal_dependencies = session_dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .count();
    let session_dev_dependencies = session_dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .count();
    if !session_features.is_empty()
        || session_dependencies.len() != 12
        || session_normal_dependencies != 9
        || session_dev_dependencies != 3
    {
        return Err(format!(
            "{session_name} must expose no features and exactly nine reviewed normal dependencies plus three test-only dependencies"
        ));
    }
    for (dependency_name, requirement) in [
        ("hkdf", "=0.12.4"),
        ("hmac", "=0.12.1"),
        ("rand_core", "=0.6.4"),
        ("sha2", "=0.10.9"),
        ("zeroize", "=1.9.0"),
    ] {
        validate_exact_registry_dependency(
            session_dependencies,
            session_name,
            dependency_name,
            requirement,
            None,
            false,
            &[],
        )?;
    }
    for (dependency_name, relative_path, uses_default_features) in [
        ("reticulum-device-api", "crates/device-api", false),
        (
            "reticulum-device-api-framing",
            "crates/device-api-framing",
            true,
        ),
        (
            "reticulum-device-api-handoff",
            "crates/device-api-handoff",
            true,
        ),
        (
            "reticulum-device-api-credentials",
            "crates/device-api-credentials",
            true,
        ),
    ] {
        validate_exact_local_dependency(
            session_dependencies,
            session_name,
            dependency_name,
            &workspace.join(relative_path),
            uses_default_features,
        )?;
    }
    validate_exact_registry_dependency(
        session_dependencies,
        session_name,
        "hex",
        "=0.4.3",
        Some("dev"),
        true,
        &[],
    )?;
    validate_exact_local_dependency_with_kind(
        session_dependencies,
        session_name,
        "reticulum-device-api-adapter",
        &workspace.join("crates/device-api-adapter"),
        Some("dev"),
        true,
        &["experimental-rns-data"],
    )?;
    validate_exact_local_dependency_with_kind(
        session_dependencies,
        session_name,
        "reticulum-storage-model",
        &workspace.join("crates/storage-model"),
        Some("dev"),
        true,
        &[],
    )?;

    Ok(())
}

fn validate_portable_durability_dependency_boundaries(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    const IDENTITY_DEPENDENCIES: [(&str, &str); 4] = [
        ("embedded-storage", "=0.3.1"),
        ("rand_core", "=0.6.4"),
        ("sha2", "=0.10.9"),
        ("zeroize", "=1.9.0"),
    ];
    const ANNOUNCE_CLOCK_DEPENDENCIES: [(&str, &str); 2] =
        [("embedded-storage", "=0.3.1"), ("sha2", "=0.10.9")];
    const NOR_REGION_DEPENDENCIES: [(&str, &str); 1] = [("embedded-storage", "=0.3.1")];
    const CREDENTIAL_STORE_REGISTRY_DEPENDENCIES: [(&str, &str); 3] = [
        ("embedded-storage", "=0.3.1"),
        ("sha2", "=0.10.9"),
        ("zeroize", "=1.9.0"),
    ];

    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;

    for (package_name, manifest, empty_default_feature, expected_dependencies) in [
        (
            "reticulum-device-identity-store",
            "crates/device-identity-store/Cargo.toml",
            false,
            &IDENTITY_DEPENDENCIES[..],
        ),
        (
            "reticulum-announce-clock",
            "crates/announce-clock/Cargo.toml",
            false,
            &ANNOUNCE_CLOCK_DEPENDENCIES[..],
        ),
        (
            "reticulum-nor-flash-region",
            "crates/nor-flash-region/Cargo.toml",
            true,
            &NOR_REGION_DEPENDENCIES[..],
        ),
    ] {
        let package = exact_local_package(packages, workspace, package_name, manifest)?;
        let features = package["features"]
            .as_object()
            .ok_or_else(|| format!("{package_name} package has no feature map"))?;
        let feature_shape_is_exact = if empty_default_feature {
            features.len() == 1
                && features
                    .get("default")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|values| values.is_empty())
        } else {
            features.is_empty()
        };
        if !feature_shape_is_exact {
            return Err(format!(
                "{package_name} exposes an unreviewed feature surface"
            ));
        }

        let dependencies = package["dependencies"]
            .as_array()
            .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
        if dependencies.len() != expected_dependencies.len() {
            return Err(format!(
                "{package_name} must have exactly {} reviewed normal dependencies, found {}",
                expected_dependencies.len(),
                dependencies.len()
            ));
        }
        for (dependency_name, requirement) in expected_dependencies {
            validate_exact_registry_dependency(
                dependencies,
                package_name,
                dependency_name,
                requirement,
                None,
                false,
                &[],
            )?;
        }
    }

    let store_name = "reticulum-device-api-credential-store";
    let store = exact_local_package(
        packages,
        workspace,
        store_name,
        "crates/device-api-credential-store/Cargo.toml",
    )?;
    let store_features = store["features"]
        .as_object()
        .ok_or_else(|| format!("{store_name} package has no feature map"))?;
    if store_features.len() != 1
        || store_features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| !values.is_empty())
    {
        return Err(format!(
            "{store_name} exposes an unreviewed feature surface"
        ));
    }
    let store_dependencies = store["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{store_name} package has no dependency array"))?;
    if store_dependencies.len() != 4
        || store_dependencies
            .iter()
            .any(|dependency| !dependency["kind"].is_null())
    {
        return Err(format!(
            "{store_name} must have exactly four reviewed normal dependencies"
        ));
    }
    validate_exact_local_dependency(
        store_dependencies,
        store_name,
        "reticulum-device-api-credentials",
        &workspace.join("crates/device-api-credentials"),
        false,
    )?;
    for (dependency_name, requirement) in CREDENTIAL_STORE_REGISTRY_DEPENDENCIES {
        validate_exact_registry_dependency(
            store_dependencies,
            store_name,
            dependency_name,
            requirement,
            None,
            false,
            &[],
        )?;
    }

    Ok(())
}

fn validate_rns_inbox_store_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let package_name = "reticulum-rns-inbox-store";
    let package = exact_local_package(
        packages,
        workspace,
        package_name,
        "crates/rns-inbox-store/Cargo.toml",
    )?;
    let features = package["features"]
        .as_object()
        .ok_or_else(|| format!("{package_name} package has no feature map"))?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| !values.is_empty())
    {
        return Err(format!(
            "{package_name} must expose only one empty default feature"
        ));
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
    if dependencies.len() != 2
        || dependencies
            .iter()
            .any(|dependency| !dependency["kind"].is_null())
    {
        return Err(format!(
            "{package_name} must have exactly two reviewed normal dependencies and no build or test dependencies"
        ));
    }
    for (dependency_name, requirement) in [("embedded-storage", "=0.3.1"), ("sha2", "=0.10.9")] {
        validate_exact_registry_dependency(
            dependencies,
            package_name,
            dependency_name,
            requirement,
            None,
            false,
            &[],
        )?;
    }
    Ok(())
}

fn validate_tracker_radio_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;

    let radio_name = "reticulum-board-heltec-tracker-v2-radio";
    let radio_manifest = workspace
        .join("crates/board-heltec-tracker-v2-radio")
        .join("Cargo.toml");
    let radios = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some(radio_name)
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(radio_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if radios.len() != 1 {
        return Err(format!(
            "expected exactly one local {radio_name} package at {}, found {}",
            radio_manifest.display(),
            radios.len()
        ));
    }
    let radio = radios[0];
    let features = radio["features"]
        .as_object()
        .ok_or_else(|| format!("{radio_name} package has no feature map"))?;
    if features.len() != 2
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| !values.is_empty())
        || features
            .get("near-field-attenuation")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| !values.is_empty())
    {
        return Err(format!(
            "{radio_name} must expose only empty default and explicit near-field-attenuation features"
        ));
    }
    let dependencies = radio["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{radio_name} package has no dependency array"))?;
    if dependencies.len() != 8 {
        return Err(format!(
            "{radio_name} must have exactly seven reviewed normal dependencies and one test-only critical-section edge, found {}",
            dependencies.len()
        ));
    }
    for (name, requirement, uses_default_features) in [
        ("critical-section", "=1.2.0", false),
        ("embedded-hal", "=1.0.0", true),
        ("embedded-hal-async", "=1.0.0", true),
        ("lora-phy", "=3.0.1", false),
    ] {
        let matches = dependencies
            .iter()
            .filter(|dependency| {
                dependency["name"].as_str() == Some(name) && dependency["kind"].is_null()
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(format!(
                "{radio_name} must have exactly one normal {name} dependency"
            ));
        }
        let dependency = matches[0];
        if dependency["req"].as_str() != Some(requirement)
            || dependency["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || !dependency["path"].is_null()
            || dependency["optional"].as_bool() != Some(false)
            || !dependency["rename"].is_null()
            || !dependency["target"].is_null()
            || dependency["uses_default_features"].as_bool() != Some(uses_default_features)
            || dependency["features"]
                .as_array()
                .is_none_or(|values| !values.is_empty())
        {
            return Err(format!(
                "{radio_name} has unreviewed normal {name} dependency shape"
            ));
        }
    }
    for (name, relative_path, uses_default_features) in [
        (
            "reticulum-board-heltec-tracker-v2",
            "crates/board-heltec-tracker-v2",
            true,
        ),
        ("reticulum-radio-interface", "crates/radio-interface", true),
        ("reticulum-radio-lora-phy", "crates/radio-lora-phy", false),
    ] {
        let expected_path = workspace.join(relative_path);
        let matches = dependencies
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(format!(
                "{radio_name} must have exactly one local {name} dependency"
            ));
        }
        let dependency = matches[0];
        if dependency["req"].as_str() != Some("*")
            || !dependency["source"].is_null()
            || dependency["path"].as_str().map(Path::new) != Some(expected_path.as_path())
            || !dependency["kind"].is_null()
            || dependency["optional"].as_bool() != Some(false)
            || !dependency["rename"].is_null()
            || !dependency["target"].is_null()
            || dependency["uses_default_features"].as_bool() != Some(uses_default_features)
            || dependency["features"]
                .as_array()
                .is_none_or(|values| !values.is_empty())
        {
            return Err(format!(
                "{radio_name} has unreviewed local {name} dependency shape"
            ));
        }
    }
    let dev_critical = dependencies
        .iter()
        .filter(|dependency| {
            dependency["name"].as_str() == Some("critical-section")
                && dependency["kind"].as_str() == Some("dev")
        })
        .collect::<Vec<_>>();
    if dev_critical.len() != 1 {
        return Err(format!(
            "{radio_name} must have exactly one test-only critical-section dependency"
        ));
    }
    let dev_critical = dev_critical[0];
    if dev_critical["req"].as_str() != Some("=1.2.0")
        || dev_critical["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !dev_critical["path"].is_null()
        || dev_critical["optional"].as_bool() != Some(false)
        || !dev_critical["rename"].is_null()
        || !dev_critical["target"].is_null()
        || dev_critical["uses_default_features"].as_bool() != Some(false)
        || dev_critical["features"]
            .as_array()
            .is_none_or(|values| values.len() != 1 || values[0].as_str() != Some("std"))
    {
        return Err(format!(
            "{radio_name} has unreviewed test-only critical-section dependency shape"
        ));
    }

    let facade_name = "reticulum-board-heltec-tracker-v2-tx-hil";
    let facade_manifest = workspace
        .join("crates/board-heltec-tracker-v2-tx-hil")
        .join("Cargo.toml");
    let facades = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some(facade_name)
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(facade_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if facades.len() != 1 {
        return Err(format!(
            "expected exactly one local {facade_name} package at {}, found {}",
            facade_manifest.display(),
            facades.len()
        ));
    }
    let facade = facades[0];
    let facade_features = facade["features"]
        .as_object()
        .ok_or_else(|| format!("{facade_name} package has no feature map"))?;
    if facade_features.len() != 2
        || facade_features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| !values.is_empty())
        || facade_features
            .get("near-field-attenuation-hil")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| {
                values.len() != 1
                    || values[0].as_str()
                        != Some("reticulum-board-heltec-tracker-v2-radio/near-field-attenuation")
            })
    {
        return Err(format!(
            "{facade_name} must expose only its exact product-radio feature forward"
        ));
    }
    let facade_dependencies = facade["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{facade_name} package has no dependency array"))?;
    let radio_path = workspace.join("crates/board-heltec-tracker-v2-radio");
    if facade_dependencies.len() != 1 {
        return Err(format!(
            "{facade_name} must have exactly one product-radio dependency, found {}",
            facade_dependencies.len()
        ));
    }
    let dependency = &facade_dependencies[0];
    if dependency["name"].as_str() != Some(radio_name)
        || dependency["req"].as_str() != Some("*")
        || !dependency["source"].is_null()
        || dependency["path"].as_str().map(Path::new) != Some(radio_path.as_path())
        || !dependency["kind"].is_null()
        || dependency["optional"].as_bool() != Some(false)
        || !dependency["rename"].is_null()
        || !dependency["target"].is_null()
        || dependency["uses_default_features"].as_bool() != Some(true)
        || dependency["features"]
            .as_array()
            .is_none_or(|values| !values.is_empty())
    {
        return Err(format!(
            "{facade_name} has an unreviewed product-radio dependency shape"
        ));
    }

    Ok(())
}

fn validate_tx_handoff_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/tx-handoff/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-tx-handoff")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-tx-handoff package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }

    let dependencies = matching[0]["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-tx-handoff package has no dependency array".to_owned())?;
    let normal = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .collect::<Vec<_>>();
    if normal.len() != 2 {
        return Err(format!(
            "reticulum-tx-handoff must have exactly two normal dependencies, found {}",
            normal.len()
        ));
    }

    let node_path = workspace.join("crates/node-core");
    let node = normal
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("reticulum-node-core"))
        .collect::<Vec<_>>();
    if node.len() != 1
        || node[0]["path"].as_str().map(Path::new) != Some(node_path.as_path())
        || !node[0]["source"].is_null()
        || node[0]["optional"].as_bool() != Some(false)
        || !node[0]["rename"].is_null()
        || !node[0]["target"].is_null()
        || node[0]["uses_default_features"].as_bool() != Some(false)
        || node[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "reticulum-node-core must be one unconditional local normal dependency with no features"
                .to_owned(),
        );
    }

    let embassy = normal
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("embassy-sync"))
        .collect::<Vec<_>>();
    if embassy.len() != 1
        || embassy[0]["req"].as_str() != Some("=0.8.0")
        || embassy[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !embassy[0]["path"].is_null()
        || embassy[0]["optional"].as_bool() != Some(false)
        || !embassy[0]["rename"].is_null()
        || !embassy[0]["target"].is_null()
        || embassy[0]["uses_default_features"].as_bool() != Some(false)
        || embassy[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "embassy-sync must be the unconditional feature-free registry =0.8.0 normal dependency"
                .to_owned(),
        );
    }

    let expected_dev = [
        ("embassy-futures", "=0.1.2"),
        ("rand_core", "=0.6.4"),
        ("static_cell", "=2.1.1"),
    ];
    let development = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .collect::<Vec<_>>();
    if development.len() != expected_dev.len() {
        return Err(format!(
            "reticulum-tx-handoff must have exactly {} reviewed dev dependencies, found {}",
            expected_dev.len(),
            development.len()
        ));
    }
    for (name, requirement) in expected_dev {
        let dependency = development
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some(requirement)
            || dependency[0]["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
        {
            return Err(format!(
                "reticulum-tx-handoff dev dependency {name} does not match the reviewed pin"
            ));
        }
    }
    if dependencies
        .iter()
        .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err("reticulum-tx-handoff must not have build dependencies".to_owned());
    }

    Ok(())
}

fn validate_tx_dispatch_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/tx-dispatch/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-tx-dispatch")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-tx-dispatch package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-tx-dispatch package has no feature map".to_owned())?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|default| !default.is_empty())
    {
        return Err("reticulum-tx-dispatch must expose only an empty default feature".to_owned());
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-tx-dispatch package has no dependency array".to_owned())?;
    if dependencies
        .iter()
        .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err("reticulum-tx-dispatch must not have build dependencies".to_owned());
    }
    if dependencies.len() != 7 {
        return Err(format!(
            "reticulum-tx-dispatch must have exactly seven reviewed dependencies, found {}",
            dependencies.len()
        ));
    }

    let normal = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .collect::<Vec<_>>();
    if normal.len() != 5 {
        return Err(format!(
            "reticulum-tx-dispatch must have exactly five normal dependencies, found {}",
            normal.len()
        ));
    }

    for (name, relative_path) in [
        ("reticulum-node-core", "crates/node-core"),
        ("reticulum-tx-handoff", "crates/tx-handoff"),
    ] {
        let expected_path = workspace.join(relative_path);
        let dependency = normal
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some("*")
            || dependency[0]["path"].as_str().map(Path::new) != Some(expected_path.as_path())
            || !dependency[0]["source"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be one unconditional feature-free local normal dependency at {}",
                expected_path.display()
            ));
        }
    }

    let embassy = normal
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("embassy-sync"))
        .collect::<Vec<_>>();
    if embassy.len() != 1
        || embassy[0]["req"].as_str() != Some("=0.8.0")
        || embassy[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !embassy[0]["path"].is_null()
        || embassy[0]["optional"].as_bool() != Some(false)
        || !embassy[0]["rename"].is_null()
        || !embassy[0]["target"].is_null()
        || embassy[0]["uses_default_features"].as_bool() != Some(false)
        || embassy[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "embassy-sync must be the unconditional feature-free registry =0.8.0 normal dependency"
                .to_owned(),
        );
    }

    let rand_core = normal
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("rand_core"))
        .collect::<Vec<_>>();
    if rand_core.len() != 1
        || rand_core[0]["req"].as_str() != Some("=0.6.4")
        || rand_core[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !rand_core[0]["path"].is_null()
        || rand_core[0]["optional"].as_bool() != Some(false)
        || !rand_core[0]["rename"].is_null()
        || !rand_core[0]["target"].is_null()
        || rand_core[0]["uses_default_features"].as_bool() != Some(false)
        || rand_core[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "rand_core must be the unconditional feature-free registry =0.6.4 normal dependency"
                .to_owned(),
        );
    }

    let sha2 = normal
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("sha2"))
        .collect::<Vec<_>>();
    if sha2.len() != 1
        || sha2[0]["req"].as_str() != Some("=0.10.9")
        || sha2[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !sha2[0]["path"].is_null()
        || sha2[0]["optional"].as_bool() != Some(false)
        || !sha2[0]["rename"].is_null()
        || !sha2[0]["target"].is_null()
        || sha2[0]["uses_default_features"].as_bool() != Some(false)
        || sha2[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "sha2 must be the unconditional feature-free registry =0.10.9 normal dependency"
                .to_owned(),
        );
    }

    let development = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .collect::<Vec<_>>();
    let expected_dev = [
        ("embassy-futures", "=0.1.2", false),
        ("static_cell", "=2.1.1", true),
    ];
    if development.len() != expected_dev.len() {
        return Err(format!(
            "reticulum-tx-dispatch must have exactly {} reviewed dev dependencies, found {}",
            expected_dev.len(),
            development.len()
        ));
    }
    for (name, requirement, uses_default_features) in expected_dev {
        let dependency = development
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some(requirement)
            || dependency[0]["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || !dependency[0]["path"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(uses_default_features)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "reticulum-tx-dispatch dev dependency {name} does not match the reviewed pin"
            ));
        }
    }

    Ok(())
}

fn validate_radio_interface_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/radio-interface/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-radio-interface")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-radio-interface package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-radio-interface package has no feature map".to_owned())?;
    if !features.is_empty() {
        return Err("reticulum-radio-interface must not expose Cargo features".to_owned());
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-radio-interface package has no dependency array".to_owned())?;
    if dependencies
        .iter()
        .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err("reticulum-radio-interface must not have build dependencies".to_owned());
    }
    if dependencies.len() != 3 {
        return Err(format!(
            "reticulum-radio-interface must have exactly three reviewed dependencies, found {}",
            dependencies.len()
        ));
    }

    let normal = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .collect::<Vec<_>>();
    if normal.len() != 2 {
        return Err(format!(
            "reticulum-radio-interface must have exactly two normal dependencies, found {}",
            normal.len()
        ));
    }

    let lora_modulation = normal
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("lora-modulation"))
        .collect::<Vec<_>>();
    if lora_modulation.len() != 1
        || lora_modulation[0]["req"].as_str() != Some("=0.1.5")
        || lora_modulation[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !lora_modulation[0]["path"].is_null()
        || lora_modulation[0]["optional"].as_bool() != Some(false)
        || !lora_modulation[0]["rename"].is_null()
        || !lora_modulation[0]["target"].is_null()
        || lora_modulation[0]["uses_default_features"].as_bool() != Some(false)
        || lora_modulation[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "lora-modulation must be the unconditional feature-free registry =0.1.5 normal dependency"
                .to_owned(),
        );
    }

    let expected_conformance_path = workspace.join("crates/rns-conformance");
    let conformance = normal
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("reticulum-rns-conformance"))
        .collect::<Vec<_>>();
    if conformance.len() != 1
        || conformance[0]["req"].as_str() != Some("*")
        || conformance[0]["path"].as_str().map(Path::new)
            != Some(expected_conformance_path.as_path())
        || !conformance[0]["source"].is_null()
        || conformance[0]["optional"].as_bool() != Some(false)
        || !conformance[0]["rename"].is_null()
        || !conformance[0]["target"].is_null()
        || conformance[0]["uses_default_features"].as_bool() != Some(true)
        || conformance[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(format!(
            "reticulum-rns-conformance must be the unconditional default-featured local normal dependency at {}",
            expected_conformance_path.display()
        ));
    }

    let development = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .collect::<Vec<_>>();
    if development.len() != 1 {
        return Err(format!(
            "reticulum-radio-interface must have exactly one reviewed development dependency, found {}",
            development.len()
        ));
    }
    let embassy_sync = development
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("embassy-sync"))
        .collect::<Vec<_>>();
    if embassy_sync.len() != 1
        || embassy_sync[0]["req"].as_str() != Some("=0.8.0")
        || embassy_sync[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !embassy_sync[0]["path"].is_null()
        || embassy_sync[0]["optional"].as_bool() != Some(false)
        || !embassy_sync[0]["rename"].is_null()
        || !embassy_sync[0]["target"].is_null()
        || embassy_sync[0]["uses_default_features"].as_bool() != Some(false)
        || embassy_sync[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "embassy-sync must be the unconditional feature-free registry =0.8.0 development dependency"
                .to_owned(),
        );
    }

    Ok(())
}

fn validate_e290_board_facts_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let package_name = "reticulum-board-heltec-vision-master-e290";
    let expected_manifest = workspace
        .join("crates/board-heltec-vision-master-e290")
        .join("Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some(package_name)
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local {package_name} package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];
    let features = package["features"]
        .as_object()
        .ok_or_else(|| format!("{package_name} package has no feature map"))?;
    if !features.is_empty() {
        return Err(format!("{package_name} must not expose Cargo features"));
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
    if dependencies.len() != 1 {
        return Err(format!(
            "{package_name} must have exactly one reviewed normal dependency, found {}",
            dependencies.len()
        ));
    }
    let dependency = &dependencies[0];
    let expected_path = workspace.join("crates/radio-interface");
    if dependency["name"].as_str() != Some("reticulum-radio-interface")
        || dependency["req"].as_str() != Some("*")
        || !dependency["source"].is_null()
        || dependency["path"].as_str().map(Path::new) != Some(expected_path.as_path())
        || !dependency["kind"].is_null()
        || dependency["optional"].as_bool() != Some(false)
        || !dependency["rename"].is_null()
        || !dependency["target"].is_null()
        || dependency["uses_default_features"].as_bool() != Some(false)
        || dependency["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(format!(
            "{package_name} must use one unconditional feature-free local reticulum-radio-interface dependency at {}",
            expected_path.display()
        ));
    }

    Ok(())
}

fn validate_semantic_hil_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let package_name = "reticulum-semantic-roundtrip-hil";
    let package = exact_local_package(
        packages,
        workspace,
        package_name,
        "crates/semantic-roundtrip-hil/Cargo.toml",
    )?;
    let features = package["features"]
        .as_object()
        .ok_or_else(|| format!("{package_name} package has no feature map"))?;
    let expected_features = serde_json::json!({
        "default": [],
        "semantic-announce-hil": [
            "dep:rand_core",
            "dep:reticulum-rns-rete",
            "reticulum-rns-rete/conformance"
        ],
        "semantic-roundtrip-hil": [
            "dep:rand_core",
            "dep:reticulum-rns-rete"
        ]
    });
    if serde_json::Value::Object(features.clone()) != expected_features {
        return Err(format!(
            "{package_name} exposes an unreviewed feature surface"
        ));
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
    if dependencies.len() != 3 {
        return Err(format!(
            "{package_name} must have exactly three reviewed dependencies, found {}",
            dependencies.len()
        ));
    }
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-radio-interface",
        &workspace.join("crates/radio-interface"),
        false,
    )?;

    let random = exact_dependency(dependencies, package_name, "rand_core", None)?;
    if random["req"].as_str() != Some("=0.6.4")
        || random["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !random["path"].is_null()
        || random["optional"].as_bool() != Some(true)
        || !random["rename"].is_null()
        || !random["target"].is_null()
        || random["uses_default_features"].as_bool() != Some(false)
        || random["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(format!(
            "{package_name} must use one optional feature-free rand_core =0.6.4 dependency"
        ));
    }

    let rns = exact_dependency(dependencies, package_name, "reticulum-rns-rete", None)?;
    let expected_rns_path = workspace.join("crates/rns-rete");
    if rns["req"].as_str() != Some("*")
        || !rns["source"].is_null()
        || rns["path"].as_str().map(Path::new) != Some(expected_rns_path.as_path())
        || rns["optional"].as_bool() != Some(true)
        || !rns["rename"].is_null()
        || !rns["target"].is_null()
        || rns["uses_default_features"].as_bool() != Some(false)
        || rns["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(format!(
            "{package_name} must use one optional feature-free local reticulum-rns-rete dependency"
        ));
    }

    Ok(())
}

fn exact_dependency<'a>(
    dependencies: &'a [serde_json::Value],
    package_name: &str,
    dependency_name: &str,
    kind: Option<&str>,
) -> Result<&'a serde_json::Value, String> {
    let matching = dependencies
        .iter()
        .filter(|dependency| {
            dependency["name"].as_str() == Some(dependency_name)
                && match kind {
                    Some(kind) => dependency["kind"].as_str() == Some(kind),
                    None => dependency["kind"].is_null(),
                }
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "{package_name} must have exactly one {kind:?} {dependency_name} dependency"
        ));
    }
    Ok(matching[0])
}

fn validate_exact_registry_dependency(
    dependencies: &[serde_json::Value],
    package_name: &str,
    dependency_name: &str,
    requirement: &str,
    kind: Option<&str>,
    uses_default_features: bool,
    expected_features: &[&str],
) -> Result<(), String> {
    let dependency = exact_dependency(dependencies, package_name, dependency_name, kind)?;
    let features = dependency["features"].as_array().ok_or_else(|| {
        format!("{package_name} {dependency_name} dependency has no feature list")
    })?;
    let actual_features = features
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    if dependency["req"].as_str() != Some(requirement)
        || dependency["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !dependency["path"].is_null()
        || dependency["optional"].as_bool() != Some(false)
        || !dependency["rename"].is_null()
        || !dependency["target"].is_null()
        || dependency["uses_default_features"].as_bool() != Some(uses_default_features)
        || actual_features != expected_features
    {
        return Err(format!(
            "{package_name} has an unreviewed {kind:?} registry {dependency_name} dependency shape"
        ));
    }
    Ok(())
}

fn validate_exact_target_registry_dependency(
    dependencies: &[serde_json::Value],
    package_name: &str,
    dependency_name: &str,
    requirement: &str,
    target: &str,
    uses_default_features: bool,
    expected_features: &[&str],
) -> Result<(), String> {
    let dependency = exact_dependency(dependencies, package_name, dependency_name, None)?;
    let features = dependency["features"].as_array().ok_or_else(|| {
        format!("{package_name} {dependency_name} dependency has no feature list")
    })?;
    let actual_features = features
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    if dependency["req"].as_str() != Some(requirement)
        || dependency["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !dependency["path"].is_null()
        || dependency["optional"].as_bool() != Some(false)
        || !dependency["rename"].is_null()
        || dependency["target"].as_str() != Some(target)
        || dependency["uses_default_features"].as_bool() != Some(uses_default_features)
        || actual_features != expected_features
    {
        return Err(format!(
            "{package_name} has an unreviewed target-specific registry {dependency_name} dependency shape"
        ));
    }
    Ok(())
}

fn validate_exact_local_dependency(
    dependencies: &[serde_json::Value],
    package_name: &str,
    dependency_name: &str,
    expected_path: &Path,
    uses_default_features: bool,
) -> Result<(), String> {
    validate_exact_local_dependency_with_kind(
        dependencies,
        package_name,
        dependency_name,
        expected_path,
        None,
        uses_default_features,
        &[],
    )
}

fn validate_exact_local_dependency_with_kind(
    dependencies: &[serde_json::Value],
    package_name: &str,
    dependency_name: &str,
    expected_path: &Path,
    kind: Option<&str>,
    uses_default_features: bool,
    expected_features: &[&str],
) -> Result<(), String> {
    let dependency = exact_dependency(dependencies, package_name, dependency_name, kind)?;
    let features = dependency["features"].as_array().ok_or_else(|| {
        format!("{package_name} {dependency_name} dependency has no feature list")
    })?;
    let actual_features = features
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    if dependency["req"].as_str() != Some("*")
        || !dependency["source"].is_null()
        || dependency["path"].as_str().map(Path::new) != Some(expected_path)
        || dependency["optional"].as_bool() != Some(false)
        || !dependency["rename"].is_null()
        || !dependency["target"].is_null()
        || dependency["uses_default_features"].as_bool() != Some(uses_default_features)
        || actual_features != expected_features
    {
        return Err(format!(
            "{package_name} has an unreviewed {kind:?} local {dependency_name} dependency shape"
        ));
    }
    Ok(())
}

fn exact_local_package<'a>(
    packages: &'a [serde_json::Value],
    workspace: &Path,
    package_name: &str,
    relative_manifest: &str,
) -> Result<&'a serde_json::Value, String> {
    let expected_manifest = workspace.join(relative_manifest);
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some(package_name)
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local {package_name} package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    Ok(matching[0])
}

fn validate_interface_router_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let package_name = "reticulum-interface-router";
    let package = exact_local_package(
        packages,
        workspace,
        package_name,
        "crates/interface-router/Cargo.toml",
    )?;
    let features = package["features"]
        .as_object()
        .ok_or_else(|| format!("{package_name} package has no feature map"))?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| !values.is_empty())
    {
        return Err(format!(
            "{package_name} must expose only one empty default feature"
        ));
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
    if dependencies.len() != 4 {
        return Err(format!(
            "{package_name} must have exactly two reviewed normal and two reviewed test dependencies, found {}",
            dependencies.len()
        ));
    }
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "embassy-sync",
        "=0.8.0",
        None,
        false,
        &[],
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-node-core",
        &workspace.join("crates/node-core"),
        false,
    )?;
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "rand_core",
        "=0.6.4",
        Some("dev"),
        false,
        &[],
    )?;

    let rns = exact_dependency(
        dependencies,
        package_name,
        "reticulum-rns-rete",
        Some("dev"),
    )?;
    let expected_rns_path = workspace.join("crates/rns-rete");
    if rns["req"].as_str() != Some("*")
        || !rns["source"].is_null()
        || rns["path"].as_str().map(Path::new) != Some(expected_rns_path.as_path())
        || rns["optional"].as_bool() != Some(false)
        || !rns["rename"].is_null()
        || !rns["target"].is_null()
        || rns["uses_default_features"].as_bool() != Some(false)
        || rns["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(format!(
            "{package_name} has an unreviewed test-only RNS fixture dependency shape"
        ));
    }

    Ok(())
}

fn validate_lora_phy_radio_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let package_name = "reticulum-radio-lora-phy";
    let package = exact_local_package(
        packages,
        workspace,
        package_name,
        "crates/radio-lora-phy/Cargo.toml",
    )?;
    if package["features"]
        .as_object()
        .is_none_or(|features| !features.is_empty())
    {
        return Err(format!("{package_name} must not expose Cargo features"));
    }
    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
    if dependencies.len() != 4 {
        return Err(format!(
            "{package_name} must have exactly four reviewed normal dependencies, found {}",
            dependencies.len()
        ));
    }
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "critical-section",
        "=1.2.0",
        None,
        false,
        &[],
    )?;
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "embedded-hal-async",
        "=1.0.0",
        None,
        true,
        &[],
    )?;
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "lora-phy",
        "=3.0.1",
        None,
        false,
        &[],
    )?;
    validate_exact_local_dependency(
        dependencies,
        package_name,
        "reticulum-radio-interface",
        &workspace.join("crates/radio-interface"),
        false,
    )?;
    Ok(())
}

fn validate_e290_radio_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let package_name = "reticulum-board-heltec-vision-master-e290-radio";
    let package = exact_local_package(
        packages,
        workspace,
        package_name,
        "crates/board-heltec-vision-master-e290-radio/Cargo.toml",
    )?;
    if package["features"]
        .as_object()
        .is_none_or(|features| !features.is_empty())
    {
        return Err(format!("{package_name} must not expose Cargo features"));
    }
    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
    if dependencies.len() != 7 {
        return Err(format!(
            "{package_name} must have six reviewed normal dependencies and one test-only critical-section edge, found {}",
            dependencies.len()
        ));
    }
    for (name, requirement, defaults) in [
        ("embedded-hal", "=1.0.0", true),
        ("embedded-hal-async", "=1.0.0", true),
        ("lora-phy", "=3.0.1", false),
    ] {
        validate_exact_registry_dependency(
            dependencies,
            package_name,
            name,
            requirement,
            None,
            defaults,
            &[],
        )?;
    }
    for (name, relative_path) in [
        (
            "reticulum-board-heltec-vision-master-e290",
            "crates/board-heltec-vision-master-e290",
        ),
        ("reticulum-radio-interface", "crates/radio-interface"),
        ("reticulum-radio-lora-phy", "crates/radio-lora-phy"),
    ] {
        validate_exact_local_dependency(
            dependencies,
            package_name,
            name,
            &workspace.join(relative_path),
            false,
        )?;
    }
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "critical-section",
        "=1.2.0",
        Some("dev"),
        false,
        &["std"],
    )?;
    Ok(())
}

fn validate_radio_tx_dispatch_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/radio-tx-dispatch/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-radio-tx-dispatch")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-radio-tx-dispatch package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-radio-tx-dispatch package has no feature map".to_owned())?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|default| !default.is_empty())
    {
        return Err(
            "reticulum-radio-tx-dispatch must expose only an empty default feature".to_owned(),
        );
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-radio-tx-dispatch package has no dependency array".to_owned())?;
    if dependencies
        .iter()
        .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err("reticulum-radio-tx-dispatch must not have build dependencies".to_owned());
    }
    if dependencies.len() != 9 {
        return Err(format!(
            "reticulum-radio-tx-dispatch must have exactly nine reviewed dependencies, found {}",
            dependencies.len()
        ));
    }

    let normal = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .collect::<Vec<_>>();
    if normal.len() != 7 {
        return Err(format!(
            "reticulum-radio-tx-dispatch must have exactly seven normal dependencies, found {}",
            normal.len()
        ));
    }

    for (name, relative_path) in [
        ("reticulum-interface-router", "crates/interface-router"),
        ("reticulum-node-core", "crates/node-core"),
        ("reticulum-radio-interface", "crates/radio-interface"),
        ("reticulum-tx-handoff", "crates/tx-handoff"),
    ] {
        let expected_path = workspace.join(relative_path);
        let dependency = normal
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some("*")
            || dependency[0]["path"].as_str().map(Path::new) != Some(expected_path.as_path())
            || !dependency[0]["source"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be one unconditional feature-free local normal dependency at {}",
                expected_path.display()
            ));
        }
    }

    for (name, requirement) in [
        ("embassy-sync", "=0.8.0"),
        ("embassy-time", "=0.5.0"),
        ("rand_core", "=0.6.4"),
    ] {
        let dependency = normal
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some(requirement)
            || dependency[0]["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || !dependency[0]["path"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be the unconditional feature-free registry {requirement} normal dependency"
            ));
        }
    }

    let development = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .collect::<Vec<_>>();
    if development.len() != 2 {
        return Err(format!(
            "reticulum-radio-tx-dispatch must have exactly two reviewed development dependencies, found {}",
            development.len()
        ));
    }
    for (name, requirement, uses_default_features) in [
        ("embassy-futures", "=0.1.2", false),
        ("static_cell", "=2.1.1", true),
    ] {
        let dependency = development
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some(requirement)
            || dependency[0]["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || !dependency[0]["path"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(uses_default_features)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "reticulum-radio-tx-dispatch development dependency {name} does not match the reviewed pin"
            ));
        }
    }

    Ok(())
}

fn validate_lxmf_wire_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    const PACKAGE_NAME: &str = "reticulum-lxmf-wire";

    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/lxmf-wire/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some(PACKAGE_NAME)
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local {PACKAGE_NAME} package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| format!("{PACKAGE_NAME} package has no feature map"))?;
    if !features.is_empty() {
        return Err(format!(
            "{PACKAGE_NAME} must expose no Cargo feature surface"
        ));
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{PACKAGE_NAME} package has no dependency array"))?;
    if dependencies
        .iter()
        .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err(format!("{PACKAGE_NAME} must not have build dependencies"));
    }
    if dependencies.len() != 8 {
        return Err(format!(
            "{PACKAGE_NAME} must have exactly five reviewed normal and three reviewed development dependencies, found {}",
            dependencies.len()
        ));
    }

    let normal_count = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .count();
    let development_count = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .count();
    if normal_count != 5 || development_count != 3 {
        return Err(format!(
            "{PACKAGE_NAME} dependency kinds must be exactly five normal and three development edges, found {normal_count} normal and {development_count} development"
        ));
    }

    for (name, requirement, features) in [
        ("curve25519-dalek", "=4.1.3", &[][..]),
        ("ed25519-dalek", "=2.2.0", &["hazmat"][..]),
        ("hkdf", "=0.12.4", &[][..]),
        ("sha2", "=0.10.9", &[][..]),
        ("subtle", "=2.6.1", &[][..]),
    ] {
        validate_exact_registry_dependency(
            dependencies,
            PACKAGE_NAME,
            name,
            requirement,
            None,
            false,
            features,
        )?;
    }
    for (name, requirement, features) in [
        ("hex", "=0.4.3", &[][..]),
        ("serde", "=1.0.228", &["derive"][..]),
        ("serde_json", "=1.0.149", &[][..]),
    ] {
        validate_exact_registry_dependency(
            dependencies,
            PACKAGE_NAME,
            name,
            requirement,
            Some("dev"),
            true,
            features,
        )?;
    }

    Ok(())
}

fn forbidden_radio_tx_dispatch_closure_category(
    package: &serde_json::Value,
    workspace: &Path,
) -> Option<&'static str> {
    let name = package["name"].as_str()?;
    if name.starts_with("reticulum-board-") {
        return Some("board");
    }
    if name == "lora-phy"
        || name.starts_with("sx126")
        || name.starts_with("sx127")
        || name.starts_with("sx128")
        || name.starts_with("radio-sx")
        || (name.starts_with("reticulum-radio-")
            && name != "reticulum-radio-interface"
            && name != "reticulum-radio-tx-dispatch")
    {
        return Some("concrete radio driver");
    }
    if name.starts_with("esp-") || name.starts_with("esp32") {
        return Some("concrete platform");
    }
    if name.starts_with("reticulum-heltec-") {
        return Some("firmware");
    }
    if name.starts_with("reticulum-storage-") {
        return Some("project storage");
    }
    if name == "reticulum-submission-projector" {
        return Some("projector");
    }
    if name.starts_with("reticulum-device-api") {
        return Some("device API");
    }
    if name == "reticulum-tx-dispatch" {
        return Some("RF-inert dispatcher");
    }
    if name == "reticulum-tx-supervisor" {
        return Some("supervisor");
    }

    package["manifest_path"]
        .as_str()
        .and_then(|path| Path::new(path).strip_prefix(workspace).ok())
        .and_then(|relative| {
            let mut components = relative.components();
            match (components.next(), components.next()) {
                (Some(Component::Normal(first)), _) if first == OsStr::new("firmware") => {
                    Some("firmware")
                }
                (Some(Component::Normal(first)), Some(Component::Normal(second)))
                    if first == OsStr::new("crates") =>
                {
                    let crate_directory = second.to_str()?;
                    if crate_directory.starts_with("board-") {
                        Some("board")
                    } else if crate_directory.starts_with("storage-") {
                        Some("project storage")
                    } else if crate_directory == "submission-projector" {
                        Some("projector")
                    } else if crate_directory.starts_with("device-api") {
                        Some("device API")
                    } else if crate_directory == "tx-dispatch" {
                        Some("RF-inert dispatcher")
                    } else if crate_directory == "tx-supervisor" {
                        Some("supervisor")
                    } else if crate_directory.starts_with("radio-")
                        && crate_directory != "radio-interface"
                        && crate_directory != "radio-tx-dispatch"
                    {
                        Some("concrete radio driver")
                    } else {
                        None
                    }
                }
                _ => None,
            }
        })
}

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";
const RETE_GIT_SOURCE: &str = "git+https://github.com/evelant/rete.git?rev=\
90570cafc812b3025011cb690ec74a27f287cb3f#\
90570cafc812b3025011cb690ec74a27f287cb3f";

#[derive(Clone, Copy)]
enum ReviewedClosureSource {
    Local(&'static str),
    Registry,
    Git(&'static str),
}

#[derive(Clone, Copy)]
struct ReviewedClosurePackage {
    name: &'static str,
    version: &'static str,
    source: ReviewedClosureSource,
    features: &'static [&'static str],
}

const fn closure_registry(
    name: &'static str,
    version: &'static str,
    features: &'static [&'static str],
) -> ReviewedClosurePackage {
    ReviewedClosurePackage {
        name,
        version,
        source: ReviewedClosureSource::Registry,
        features,
    }
}

const fn closure_git(
    name: &'static str,
    version: &'static str,
    features: &'static [&'static str],
) -> ReviewedClosurePackage {
    ReviewedClosurePackage {
        name,
        version,
        source: ReviewedClosureSource::Git(RETE_GIT_SOURCE),
        features,
    }
}

const fn closure_local(
    name: &'static str,
    manifest: &'static str,
    features: &'static [&'static str],
) -> ReviewedClosurePackage {
    ReviewedClosurePackage {
        name,
        version: "0.1.0",
        source: ReviewedClosureSource::Local(manifest),
        features,
    }
}

const LXMF_WIRE_REVIEWED_CLOSURE: [ReviewedClosurePackage; 15] = [
    closure_registry("block-buffer", "0.10.4", &[]),
    closure_registry("cfg-if", "1.0.4", &[]),
    closure_registry("crypto-common", "0.1.7", &[]),
    closure_registry("curve25519-dalek", "4.1.3", &["digest"]),
    closure_registry(
        "digest",
        "0.10.7",
        &["block-buffer", "core-api", "default", "mac", "subtle"],
    ),
    closure_registry("ed25519", "2.2.3", &[]),
    closure_registry("ed25519-dalek", "2.2.0", &["hazmat"]),
    closure_registry("generic-array", "0.14.7", &["more_lengths"]),
    closure_registry("hkdf", "0.12.4", &[]),
    closure_registry("hmac", "0.12.1", &[]),
    closure_local("reticulum-lxmf-wire", "crates/lxmf-wire/Cargo.toml", &[]),
    closure_registry("sha2", "0.10.9", &[]),
    closure_registry("signature", "2.2.0", &[]),
    closure_registry("subtle", "2.6.1", &[]),
    closure_registry("typenum", "1.20.1", &[]),
];

const RADIO_TX_DISPATCH_REVIEWED_CLOSURE: [ReviewedClosurePackage; 64] = [
    closure_registry("aes", "0.8.4", &[]),
    closure_registry("block-buffer", "0.10.4", &[]),
    closure_registry("byteorder", "1.5.0", &[]),
    closure_registry("cbc", "0.1.2", &[]),
    closure_registry("cfg-if", "1.0.4", &[]),
    closure_registry("cipher", "0.4.4", &[]),
    closure_registry("cpufeatures", "0.2.17", &[]),
    closure_registry("critical-section", "1.2.0", &[]),
    closure_registry("crypto-common", "0.1.7", &[]),
    closure_registry("curve25519-dalek", "4.1.3", &["digest", "zeroize"]),
    closure_registry("curve25519-dalek-derive", "0.1.1", &[]),
    closure_registry(
        "digest",
        "0.10.7",
        &["block-buffer", "core-api", "default", "mac", "subtle"],
    ),
    closure_registry("document-features", "0.2.12", &["default"]),
    closure_registry("ed25519", "2.2.3", &[]),
    closure_registry("ed25519-dalek", "2.2.0", &["zeroize"]),
    closure_registry("embassy-sync", "0.8.0", &[]),
    closure_registry("embassy-time", "0.5.0", &[]),
    closure_registry("embassy-time-driver", "0.2.2", &[]),
    closure_registry("embedded-hal", "0.2.7", &[]),
    closure_registry("embedded-hal", "1.0.0", &[]),
    closure_registry("embedded-hal-async", "1.0.0", &[]),
    closure_registry("embedded-io", "0.6.1", &[]),
    closure_registry("embedded-io", "0.7.1", &[]),
    closure_registry("embedded-io-async", "0.6.1", &[]),
    closure_registry("embedded-io-async", "0.7.0", &[]),
    closure_registry("fiat-crypto", "0.2.9", &[]),
    closure_registry("futures-core", "0.3.32", &[]),
    closure_registry("futures-sink", "0.3.32", &[]),
    closure_registry("generic-array", "0.14.7", &["more_lengths"]),
    closure_registry("hash32", "0.3.1", &[]),
    closure_registry("heapless", "0.8.0", &[]),
    closure_registry("heapless", "0.9.3", &[]),
    closure_registry("hkdf", "0.12.4", &[]),
    closure_registry("hmac", "0.12.1", &[]),
    closure_registry("inout", "0.1.4", &[]),
    closure_registry("libc", "0.2.186", &[]),
    closure_registry("litrs", "1.0.0", &[]),
    closure_registry("lora-modulation", "0.1.5", &[]),
    closure_registry("nb", "0.1.3", &[]),
    closure_registry("nb", "1.1.0", &[]),
    closure_registry("proc-macro2", "1.0.106", &["default", "proc-macro"]),
    closure_registry("quote", "1.0.46", &["default", "proc-macro"]),
    closure_registry("rand_core", "0.6.4", &[]),
    closure_git("rete-core", "0.1.0", &["alloc", "default"]),
    closure_git("rete-stack", "0.1.0", &["alloc"]),
    closure_git("rete-transport", "0.1.0", &[]),
    closure_local(
        "reticulum-interface-router",
        "crates/interface-router/Cargo.toml",
        &[],
    ),
    closure_local("reticulum-node-core", "crates/node-core/Cargo.toml", &[]),
    closure_local(
        "reticulum-radio-interface",
        "crates/radio-interface/Cargo.toml",
        &[],
    ),
    closure_local(
        "reticulum-radio-tx-dispatch",
        "crates/radio-tx-dispatch/Cargo.toml",
        &["default"],
    ),
    closure_local(
        "reticulum-rns-conformance",
        "crates/rns-conformance/Cargo.toml",
        &[],
    ),
    closure_local("reticulum-rns-rete", "crates/rns-rete/Cargo.toml", &[]),
    closure_local("reticulum-tx-handoff", "crates/tx-handoff/Cargo.toml", &[]),
    closure_registry("sha2", "0.10.9", &[]),
    closure_registry("signature", "2.2.0", &[]),
    closure_registry("stable_deref_trait", "1.2.1", &[]),
    closure_registry("subtle", "2.6.1", &[]),
    closure_registry(
        "syn",
        "2.0.118",
        &[
            "clone-impls",
            "default",
            "derive",
            "extra-traits",
            "full",
            "parsing",
            "printing",
            "proc-macro",
            "visit",
        ],
    ),
    closure_registry("typenum", "1.20.1", &[]),
    closure_registry("unicode-ident", "1.0.24", &[]),
    closure_registry("void", "1.0.2", &[]),
    closure_registry("x25519-dalek", "2.0.1", &["static_secrets", "zeroize"]),
    closure_registry("zeroize", "1.9.0", &["derive", "zeroize_derive"]),
    closure_registry("zeroize_derive", "1.5.0", &[]),
];

fn reviewed_closure_display(
    package: ReviewedClosurePackage,
    workspace: &Path,
) -> Result<String, String> {
    let base = format!("{} v{}", package.name, package.version);
    match package.source {
        ReviewedClosureSource::Local(relative_manifest) => {
            let crate_directory = workspace
                .join(relative_manifest)
                .parent()
                .ok_or_else(|| format!("reviewed manifest {relative_manifest} has no parent"))?
                .to_owned();
            Ok(format!("{base} ({})", crate_directory.display()))
        }
        ReviewedClosureSource::Registry => Ok(base),
        ReviewedClosureSource::Git(source) => {
            let source = source
                .strip_prefix("git+")
                .ok_or_else(|| format!("reviewed git source for {} lacks git+", package.name))?;
            let (url, revision) = source.rsplit_once('#').ok_or_else(|| {
                format!("reviewed git source for {} lacks a revision", package.name)
            })?;
            let short_revision = revision.get(..8).ok_or_else(|| {
                format!("reviewed git revision for {} is too short", package.name)
            })?;
            Ok(format!("{base} ({url}#{short_revision})"))
        }
    }
}

fn metadata_matches_reviewed_closure_package(
    package: &serde_json::Value,
    reviewed: ReviewedClosurePackage,
    workspace: &Path,
) -> bool {
    if package["name"].as_str() != Some(reviewed.name)
        || package["version"].as_str() != Some(reviewed.version)
    {
        return false;
    }
    match reviewed.source {
        ReviewedClosureSource::Local(relative_manifest) => {
            package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(workspace.join(relative_manifest).as_path())
        }
        ReviewedClosureSource::Registry => package["source"].as_str() == Some(CRATES_IO_SOURCE),
        ReviewedClosureSource::Git(source) => package["source"].as_str() == Some(source),
    }
}

fn validate_reviewed_resolved_closure(
    metadata_json: &str,
    cargo_tree: &str,
    workspace: &Path,
    reviewed_closure: &[ReviewedClosurePackage],
    boundary: &str,
    forbidden_category: Option<fn(&serde_json::Value, &Path) -> Option<&'static str>>,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;

    let expected_displays = reviewed_closure
        .iter()
        .copied()
        .map(|package| reviewed_closure_display(package, workspace))
        .collect::<Result<Vec<_>, _>>()?;
    let mut seen = BTreeSet::new();
    for (line_index, line) in cargo_tree.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let (display, feature_text) = line.split_once('\t').ok_or_else(|| {
            format!(
                "resolved closure line {} has no feature separator: {line:?}",
                line_index + 1
            )
        })?;
        let display = display.strip_suffix(" (proc-macro)").unwrap_or(display);
        let reviewed_indices = expected_displays
            .iter()
            .enumerate()
            .filter_map(|(index, expected)| (expected == display).then_some(index))
            .collect::<Vec<_>>();
        if reviewed_indices.len() != 1 {
            let mut words = display.split_whitespace();
            let name = words.next().unwrap_or("<unknown>");
            let version = words
                .next()
                .and_then(|version| version.strip_prefix('v'))
                .unwrap_or("<unknown>");
            let category = forbidden_category.and_then(|classify| {
                packages
                    .iter()
                    .filter(|package| {
                        package["name"].as_str() == Some(name)
                            && package["version"].as_str() == Some(version)
                    })
                    .find_map(|package| classify(package, workspace))
            });
            return Err(if let Some(category) = category {
                format!(
                    "{boundary} resolved normal closure contains unreviewed {category} package identity {display}"
                )
            } else {
                format!(
                    "{boundary} resolved normal closure contains unreviewed package identity {display}"
                )
            });
        }
        let reviewed_index = reviewed_indices[0];
        let reviewed = reviewed_closure[reviewed_index];
        let matching_metadata = packages
            .iter()
            .filter(|package| {
                metadata_matches_reviewed_closure_package(package, reviewed, workspace)
            })
            .count();
        if matching_metadata != 1 {
            return Err(format!(
                "reviewed closure identity {display} must have exactly one matching metadata package, found {matching_metadata}"
            ));
        }

        let actual_features = if feature_text.is_empty() {
            BTreeSet::new()
        } else {
            let features = feature_text.split(',').collect::<BTreeSet<_>>();
            if features.len() != feature_text.split(',').count() {
                return Err(format!(
                    "resolved closure identity {display} repeats an enabled feature"
                ));
            }
            features
        };
        let expected_features = reviewed.features.iter().copied().collect::<BTreeSet<_>>();
        if actual_features != expected_features {
            return Err(format!(
                "resolved closure identity {display} enables features {actual_features:?}, expected exactly {expected_features:?}"
            ));
        }
        seen.insert(reviewed_index);
    }

    if seen.len() != reviewed_closure.len() {
        let missing = reviewed_closure
            .iter()
            .enumerate()
            .filter(|(index, _package)| !seen.contains(index))
            .map(|(_index, package)| format!("{} v{}", package.name, package.version))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!(
            "resolved normal closure is missing reviewed package identities: {missing}"
        ));
    }

    Ok(())
}

fn validate_radio_tx_dispatch_resolved_closure(
    metadata_json: &str,
    cargo_tree: &str,
    workspace: &Path,
) -> Result<(), String> {
    validate_reviewed_resolved_closure(
        metadata_json,
        cargo_tree,
        workspace,
        &RADIO_TX_DISPATCH_REVIEWED_CLOSURE,
        "radio TX dispatcher",
        Some(forbidden_radio_tx_dispatch_closure_category),
    )
}

fn validate_lxmf_wire_resolved_closure(
    metadata_json: &str,
    cargo_tree: &str,
    workspace: &Path,
) -> Result<(), String> {
    validate_reviewed_resolved_closure(
        metadata_json,
        cargo_tree,
        workspace,
        &LXMF_WIRE_REVIEWED_CLOSURE,
        "LXMF wire",
        None,
    )
}

fn validate_storage_model_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/storage-model/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-storage-model")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-storage-model package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-storage-model package has no feature map".to_owned())?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|default| !default.is_empty())
    {
        return Err("reticulum-storage-model must expose only an empty default feature".to_owned());
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-storage-model package has no dependency array".to_owned())?;
    if dependencies
        .iter()
        .any(|dependency| !dependency["kind"].is_null())
    {
        return Err(
            "reticulum-storage-model must not have build or development dependencies".to_owned(),
        );
    }
    if dependencies.len() != 2 {
        return Err(format!(
            "reticulum-storage-model must have exactly two reviewed normal dependencies, found {}",
            dependencies.len()
        ));
    }

    for (name, requirement) in [("minicbor", "=2.2.2"), ("sha2", "=0.10.9")] {
        let dependency = dependencies
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some(requirement)
            || dependency[0]["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || !dependency[0]["path"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be the unconditional feature-free registry {requirement} normal dependency"
            ));
        }
    }

    Ok(())
}

fn validate_storage_journal_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/storage-journal/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-storage-journal")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-storage-journal package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-storage-journal package has no feature map".to_owned())?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|default| !default.is_empty())
    {
        return Err(
            "reticulum-storage-journal must expose only an empty default feature".to_owned(),
        );
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-storage-journal package has no dependency array".to_owned())?;
    if dependencies
        .iter()
        .any(|dependency| !dependency["kind"].is_null())
    {
        return Err(
            "reticulum-storage-journal must not have build or development dependencies".to_owned(),
        );
    }
    if dependencies.len() != 3 {
        return Err(format!(
            "reticulum-storage-journal must have exactly three reviewed normal dependencies, found {}",
            dependencies.len()
        ));
    }

    for (name, requirement) in [("embedded-storage", "=0.3.1"), ("sha2", "=0.10.9")] {
        let dependency = dependencies
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some(requirement)
            || dependency[0]["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || !dependency[0]["path"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be the unconditional feature-free registry {requirement} normal dependency"
            ));
        }
    }

    let model_path = workspace.join("crates/storage-model");
    let model = dependencies
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("reticulum-storage-model"))
        .collect::<Vec<_>>();
    if model.len() != 1
        || model[0]["req"].as_str() != Some("*")
        || model[0]["path"].as_str().map(Path::new) != Some(model_path.as_path())
        || !model[0]["source"].is_null()
        || model[0]["optional"].as_bool() != Some(false)
        || !model[0]["rename"].is_null()
        || !model[0]["target"].is_null()
        || model[0]["uses_default_features"].as_bool() != Some(false)
        || model[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "reticulum-storage-model must be one unconditional feature-free local normal dependency at crates/storage-model"
                .to_owned(),
        );
    }

    Ok(())
}

fn validate_storage_actor_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/storage-actor/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-storage-actor")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-storage-actor package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-storage-actor package has no feature map".to_owned())?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|default| !default.is_empty())
    {
        return Err("reticulum-storage-actor must expose only an empty default feature".to_owned());
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-storage-actor package has no dependency array".to_owned())?;
    if dependencies
        .iter()
        .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err("reticulum-storage-actor must not have build dependencies".to_owned());
    }
    if dependencies.len() != 6 {
        return Err(format!(
            "reticulum-storage-actor must have exactly six reviewed dependencies, found {}",
            dependencies.len()
        ));
    }

    let normal = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .collect::<Vec<_>>();
    if normal.len() != 5 {
        return Err(format!(
            "reticulum-storage-actor must have exactly five normal dependencies, found {}",
            normal.len()
        ));
    }

    let embedded_storage = normal
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("embedded-storage"))
        .collect::<Vec<_>>();
    if embedded_storage.len() != 1
        || embedded_storage[0]["req"].as_str() != Some("=0.3.1")
        || embedded_storage[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !embedded_storage[0]["path"].is_null()
        || embedded_storage[0]["optional"].as_bool() != Some(false)
        || !embedded_storage[0]["rename"].is_null()
        || !embedded_storage[0]["target"].is_null()
        || embedded_storage[0]["uses_default_features"].as_bool() != Some(false)
        || embedded_storage[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "embedded-storage must be the unconditional feature-free registry =0.3.1 normal dependency"
                .to_owned(),
        );
    }

    for (name, relative_path) in [
        ("reticulum-node-core", "crates/node-core"),
        ("reticulum-storage-journal", "crates/storage-journal"),
        ("reticulum-storage-model", "crates/storage-model"),
        (
            "reticulum-submission-projector",
            "crates/submission-projector",
        ),
    ] {
        let expected_path = workspace.join(relative_path);
        let dependency = normal
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some("*")
            || dependency[0]["path"].as_str().map(Path::new) != Some(expected_path.as_path())
            || !dependency[0]["source"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be one unconditional feature-free local normal dependency at {}",
                expected_path.display()
            ));
        }
    }

    let development = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .collect::<Vec<_>>();
    if development.len() != 1 {
        return Err(format!(
            "reticulum-storage-actor must have exactly one reviewed dev dependency, found {}",
            development.len()
        ));
    }

    let rand_core = development
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("rand_core"))
        .collect::<Vec<_>>();
    if rand_core.len() != 1
        || rand_core[0]["req"].as_str() != Some("=0.6.4")
        || rand_core[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !rand_core[0]["path"].is_null()
        || rand_core[0]["optional"].as_bool() != Some(false)
        || !rand_core[0]["rename"].is_null()
        || !rand_core[0]["target"].is_null()
        || rand_core[0]["uses_default_features"].as_bool() != Some(false)
        || rand_core[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "reticulum-storage-actor dev dependency rand_core must be the unconditional feature-free registry =0.6.4 pin"
                .to_owned(),
        );
    }

    Ok(())
}

fn validate_submission_runtime_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let package_name = "reticulum-submission-runtime";
    let package = exact_local_package(
        packages,
        workspace,
        package_name,
        "crates/submission-runtime/Cargo.toml",
    )?;
    let features = package["features"]
        .as_object()
        .ok_or_else(|| format!("{package_name} package has no feature map"))?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|values| !values.is_empty())
    {
        return Err(format!(
            "{package_name} must expose only one empty default feature"
        ));
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| format!("{package_name} package has no dependency array"))?;
    if dependencies.len() != 9
        || dependencies
            .iter()
            .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err(format!(
            "{package_name} must have exactly eight reviewed normal dependencies and one reviewed test dependency"
        ));
    }
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "embassy-sync",
        "=0.8.0",
        None,
        false,
        &[],
    )?;
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "embedded-storage",
        "=0.3.1",
        None,
        false,
        &[],
    )?;
    validate_exact_registry_dependency(
        dependencies,
        package_name,
        "rand_core",
        "=0.6.4",
        None,
        false,
        &[],
    )?;
    for (name, relative_path) in [
        ("reticulum-node-core", "crates/node-core"),
        ("reticulum-storage-actor", "crates/storage-actor"),
        ("reticulum-storage-model", "crates/storage-model"),
        (
            "reticulum-submission-projector",
            "crates/submission-projector",
        ),
        ("reticulum-tx-supervisor", "crates/tx-supervisor"),
    ] {
        validate_exact_local_dependency(
            dependencies,
            package_name,
            name,
            &workspace.join(relative_path),
            false,
        )?;
    }

    let journal = exact_dependency(
        dependencies,
        package_name,
        "reticulum-storage-journal",
        Some("dev"),
    )?;
    let expected_journal_path = workspace.join("crates/storage-journal");
    if journal["req"].as_str() != Some("*")
        || !journal["source"].is_null()
        || journal["path"].as_str().map(Path::new) != Some(expected_journal_path.as_path())
        || journal["optional"].as_bool() != Some(false)
        || !journal["rename"].is_null()
        || !journal["target"].is_null()
        || journal["uses_default_features"].as_bool() != Some(false)
        || journal["features"]
            .as_array()
            .is_none_or(|values| !values.is_empty())
    {
        return Err(format!(
            "{package_name} has an unreviewed test-only storage-journal dependency shape"
        ));
    }

    Ok(())
}

fn validate_device_api_adapter_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/device-api-adapter/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-device-api-adapter")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-device-api-adapter package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-device-api-adapter package has no feature map".to_owned())?;
    let default = features
        .get("default")
        .and_then(serde_json::Value::as_array);
    let experimental_rns_data = features
        .get("experimental-rns-data")
        .and_then(serde_json::Value::as_array);
    let experimental_rns_inbox = features
        .get("experimental-rns-inbox")
        .and_then(serde_json::Value::as_array);
    if features.len() != 3
        || default.is_none_or(|default| !default.is_empty())
        || experimental_rns_data.is_none_or(|experimental_rns_data| {
            experimental_rns_data.len() != 1
                || experimental_rns_data[0].as_str()
                    != Some("reticulum-device-api/experimental-rns-data")
        })
        || experimental_rns_inbox.is_none_or(|experimental_rns_inbox| {
            experimental_rns_inbox.len() != 1
                || experimental_rns_inbox[0].as_str()
                    != Some("reticulum-device-api/experimental-rns-inbox")
        })
    {
        return Err(
            "reticulum-device-api-adapter must expose only default=[] plus exact experimental-rns-data and experimental-rns-inbox forwards"
                .to_owned(),
        );
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-device-api-adapter package has no dependency array".to_owned())?;
    if dependencies.len() != 4 {
        return Err(format!(
            "reticulum-device-api-adapter must have exactly two reviewed normal and two reviewed test dependencies, found {} total",
            dependencies.len()
        ));
    }

    let normal_count = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .count();
    let dev_count = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .count();
    if normal_count != 2
        || dev_count != 2
        || dependencies.iter().any(|dependency| {
            !dependency["kind"].is_null() && dependency["kind"].as_str() != Some("dev")
        })
    {
        return Err(format!(
            "reticulum-device-api-adapter must have exactly two normal and two dev dependencies, found normal={normal_count} dev={dev_count}"
        ));
    }

    let embedded_storage = dependencies
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("embedded-storage"))
        .collect::<Vec<_>>();
    if embedded_storage.len() != 1
        || embedded_storage[0]["req"].as_str() != Some("=0.3.1")
        || embedded_storage[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !embedded_storage[0]["path"].is_null()
        || embedded_storage[0]["optional"].as_bool() != Some(false)
        || !embedded_storage[0]["rename"].is_null()
        || !embedded_storage[0]["target"].is_null()
        || embedded_storage[0]["kind"].as_str() != Some("dev")
        || embedded_storage[0]["uses_default_features"].as_bool() != Some(false)
        || embedded_storage[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "embedded-storage must be the unconditional feature-free registry =0.3.1 dev dependency"
                .to_owned(),
        );
    }

    for (name, relative_path, expected_kind) in [
        ("reticulum-device-api", "crates/device-api", None),
        (
            "reticulum-storage-actor",
            "crates/storage-actor",
            Some("dev"),
        ),
        ("reticulum-storage-model", "crates/storage-model", None),
    ] {
        let expected_path = workspace.join(relative_path);
        let dependency = dependencies
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some("*")
            || dependency[0]["path"].as_str().map(Path::new) != Some(expected_path.as_path())
            || !dependency[0]["source"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || match expected_kind {
                Some(kind) => dependency[0]["kind"].as_str() != Some(kind),
                None => !dependency[0]["kind"].is_null(),
            }
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be one unconditional feature-free local {} dependency at {}",
                expected_kind.unwrap_or("normal"),
                expected_path.display()
            ));
        }
    }

    Ok(())
}

fn validate_storage_hil_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("firmware/heltec-tracker-v2-storage-hil/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-heltec-tracker-v2-storage-hil")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local physical-storage HIL package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];
    let features = package["features"]
        .as_object()
        .ok_or_else(|| "physical-storage HIL package has no feature map".to_owned())?;
    if !features.is_empty() {
        return Err("physical-storage HIL must not expose Cargo features".to_owned());
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "physical-storage HIL package has no dependency array".to_owned())?;
    let expected_names = BTreeSet::from([
        "embedded-storage",
        "esp-backtrace",
        "esp-bootloader-esp-idf",
        "esp-hal",
        "esp-println",
        "esp-storage",
        "log",
        "reticulum-nor-flash-region",
        "reticulum-storage-journal",
        "reticulum-storage-model",
    ]);
    let actual_names = dependencies
        .iter()
        .filter_map(|dependency| dependency["name"].as_str())
        .collect::<BTreeSet<_>>();
    if dependencies.len() != expected_names.len() || actual_names != expected_names {
        return Err(format!(
            "physical-storage HIL dependency allowlist drifted: {actual_names:?}"
        ));
    }
    if dependencies.iter().any(|dependency| {
        !dependency["kind"].is_null()
            || dependency["optional"].as_bool() != Some(false)
            || !dependency["rename"].is_null()
            || !dependency["target"].is_null()
            || dependency["uses_default_features"].as_bool() != Some(false)
    }) {
        return Err(
            "physical-storage HIL dependencies must be unconditional, normal, unrenamed and default-feature-free"
                .to_owned(),
        );
    }
    for (name, relative_path) in [
        ("reticulum-nor-flash-region", "crates/nor-flash-region"),
        ("reticulum-storage-journal", "crates/storage-journal"),
        ("reticulum-storage-model", "crates/storage-model"),
    ] {
        let expected_path = workspace.join(relative_path);
        let dependency = dependencies
            .iter()
            .find(|dependency| dependency["name"].as_str() == Some(name))
            .ok_or_else(|| format!("physical-storage HIL is missing {name}"))?;
        if dependency["path"].as_str().map(Path::new) != Some(expected_path.as_path())
            || !dependency["source"].is_null()
            || dependency["req"].as_str() != Some("*")
            || dependency["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "physical-storage HIL {name} must be a feature-free local edge resolving to {}",
                expected_path.display()
            ));
        }
    }
    let registry_dependencies: [(&str, &str, &[&str]); 7] = [
        ("embedded-storage", "=0.3.1", &[]),
        (
            "esp-backtrace",
            "=0.19.0",
            &["esp32s3", "panic-handler", "println"],
        ),
        (
            "esp-bootloader-esp-idf",
            "=0.5.0",
            &["esp32s3", "log-04", "validation"],
        ),
        (
            "esp-hal",
            "=1.1.1",
            &[
                "esp32s3",
                "exception-handler",
                "float-save-restore",
                "log-04",
                "rt",
                "unstable",
            ],
        ),
        ("esp-println", "=0.17.0", &["auto", "esp32s3", "log-04"]),
        ("esp-storage", "=0.9.0", &["critical-section", "esp32s3"]),
        ("log", "=0.4.27", &[]),
    ];
    for (name, requirement, expected_features) in registry_dependencies {
        let dependency = dependencies
            .iter()
            .find(|dependency| dependency["name"].as_str() == Some(name))
            .ok_or_else(|| format!("physical-storage HIL is missing {name}"))?;
        let actual_features = dependency["features"]
            .as_array()
            .ok_or_else(|| format!("physical-storage HIL {name} has no feature list"))?;
        let actual_features = actual_features
            .iter()
            .filter_map(serde_json::Value::as_str)
            .collect::<BTreeSet<_>>();
        let expected_features = expected_features.iter().copied().collect::<BTreeSet<_>>();
        if dependency["req"].as_str() != Some(requirement)
            || dependency["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || !dependency["path"].is_null()
            || actual_features != expected_features
        {
            return Err(format!(
                "physical-storage HIL {name} does not match reviewed registry pin {requirement} and feature set {expected_features:?}"
            ));
        }
    }
    Ok(())
}

fn validate_submission_projector_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/submission-projector/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-submission-projector")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-submission-projector package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-submission-projector package has no feature map".to_owned())?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|default| !default.is_empty())
    {
        return Err(
            "reticulum-submission-projector must expose only an empty default feature".to_owned(),
        );
    }

    let dependencies = package["dependencies"].as_array().ok_or_else(|| {
        "reticulum-submission-projector package has no dependency array".to_owned()
    })?;
    if dependencies
        .iter()
        .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err("reticulum-submission-projector must not have build dependencies".to_owned());
    }
    if dependencies.len() != 4 {
        return Err(format!(
            "reticulum-submission-projector must have exactly four reviewed dependencies, found {}",
            dependencies.len()
        ));
    }

    let normal = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .collect::<Vec<_>>();
    if normal.len() != 2 {
        return Err(format!(
            "reticulum-submission-projector must have exactly two normal dependencies, found {}",
            normal.len()
        ));
    }
    for (name, relative_path) in [
        ("reticulum-node-core", "crates/node-core"),
        ("reticulum-storage-model", "crates/storage-model"),
    ] {
        let expected_path = workspace.join(relative_path);
        let dependency = normal
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some("*")
            || dependency[0]["path"].as_str().map(Path::new) != Some(expected_path.as_path())
            || !dependency[0]["source"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be one unconditional feature-free local normal dependency at {}",
                expected_path.display()
            ));
        }
    }

    let development = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .collect::<Vec<_>>();
    if development.len() != 2 {
        return Err(format!(
            "reticulum-submission-projector must have exactly two reviewed development dependencies, found {}",
            development.len()
        ));
    }
    let rand_core = development
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("rand_core"))
        .collect::<Vec<_>>();
    if rand_core.len() != 1
        || rand_core[0]["req"].as_str() != Some("=0.6.4")
        || rand_core[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !rand_core[0]["path"].is_null()
        || rand_core[0]["optional"].as_bool() != Some(false)
        || !rand_core[0]["rename"].is_null()
        || !rand_core[0]["target"].is_null()
        || rand_core[0]["uses_default_features"].as_bool() != Some(false)
        || rand_core[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "rand_core must be one feature-free registry =0.6.4 development dependency".to_owned(),
        );
    }

    let rns_path = workspace.join("crates/rns-rete");
    let rns_rete = development
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("reticulum-rns-rete"))
        .collect::<Vec<_>>();
    if rns_rete.len() != 1
        || rns_rete[0]["req"].as_str() != Some("*")
        || rns_rete[0]["path"].as_str().map(Path::new) != Some(rns_path.as_path())
        || !rns_rete[0]["source"].is_null()
        || rns_rete[0]["optional"].as_bool() != Some(false)
        || !rns_rete[0]["rename"].is_null()
        || !rns_rete[0]["target"].is_null()
        || rns_rete[0]["uses_default_features"].as_bool() != Some(false)
        || rns_rete[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "reticulum-rns-rete must be one feature-free local development dependency at crates/rns-rete"
                .to_owned(),
        );
    }

    Ok(())
}

fn validate_tx_supervisor_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let expected_manifest = workspace.join("crates/tx-supervisor/Cargo.toml");
    let matching = packages
        .iter()
        .filter(|package| {
            package["name"].as_str() == Some("reticulum-tx-supervisor")
                && package["source"].is_null()
                && package["manifest_path"].as_str().map(Path::new)
                    == Some(expected_manifest.as_path())
        })
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one local reticulum-tx-supervisor package at {}, found {}",
            expected_manifest.display(),
            matching.len()
        ));
    }
    let package = matching[0];

    let features = package["features"]
        .as_object()
        .ok_or_else(|| "reticulum-tx-supervisor package has no feature map".to_owned())?;
    if features.len() != 1
        || features
            .get("default")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|default| !default.is_empty())
    {
        return Err("reticulum-tx-supervisor must expose only an empty default feature".to_owned());
    }

    let dependencies = package["dependencies"]
        .as_array()
        .ok_or_else(|| "reticulum-tx-supervisor package has no dependency array".to_owned())?;
    if dependencies
        .iter()
        .any(|dependency| dependency["kind"].as_str() == Some("build"))
    {
        return Err("reticulum-tx-supervisor must not have build dependencies".to_owned());
    }
    if dependencies.len() != 10 {
        return Err(format!(
            "reticulum-tx-supervisor must have exactly ten reviewed dependencies, found {}",
            dependencies.len()
        ));
    }

    let normal = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].is_null())
        .collect::<Vec<_>>();
    if normal.len() != 8 {
        return Err(format!(
            "reticulum-tx-supervisor must have exactly eight normal dependencies, found {}",
            normal.len()
        ));
    }

    for (name, relative_path) in [
        ("reticulum-interface-router", "crates/interface-router"),
        ("reticulum-node-core", "crates/node-core"),
        ("reticulum-tx-dispatch", "crates/tx-dispatch"),
        ("reticulum-tx-handoff", "crates/tx-handoff"),
    ] {
        let expected_path = workspace.join(relative_path);
        let dependency = normal
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some("*")
            || dependency[0]["path"].as_str().map(Path::new) != Some(expected_path.as_path())
            || !dependency[0]["source"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be one unconditional feature-free local normal dependency at {}",
                expected_path.display()
            ));
        }
    }

    for (name, requirement) in [
        ("embassy-sync", "=0.8.0"),
        ("embassy-futures", "=0.1.2"),
        ("embassy-time", "=0.5.0"),
        ("rand_core", "=0.6.4"),
    ] {
        let dependency = normal
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if dependency.len() != 1
            || dependency[0]["req"].as_str() != Some(requirement)
            || dependency[0]["source"].as_str()
                != Some("registry+https://github.com/rust-lang/crates.io-index")
            || !dependency[0]["path"].is_null()
            || dependency[0]["optional"].as_bool() != Some(false)
            || !dependency[0]["rename"].is_null()
            || !dependency[0]["target"].is_null()
            || dependency[0]["uses_default_features"].as_bool() != Some(false)
            || dependency[0]["features"]
                .as_array()
                .is_none_or(|features| !features.is_empty())
        {
            return Err(format!(
                "{name} must be the unconditional feature-free registry {requirement} normal dependency"
            ));
        }
    }

    let development = dependencies
        .iter()
        .filter(|dependency| dependency["kind"].as_str() == Some("dev"))
        .collect::<Vec<_>>();
    if development.len() != 2 {
        return Err(format!(
            "reticulum-tx-supervisor must have exactly two reviewed dev dependencies, found {}",
            development.len()
        ));
    }

    let rns_path = workspace.join("crates/rns-rete");
    let rns_rete = development
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("reticulum-rns-rete"))
        .collect::<Vec<_>>();
    if rns_rete.len() != 1
        || rns_rete[0]["req"].as_str() != Some("*")
        || rns_rete[0]["path"].as_str().map(Path::new) != Some(rns_path.as_path())
        || !rns_rete[0]["source"].is_null()
        || rns_rete[0]["optional"].as_bool() != Some(false)
        || !rns_rete[0]["rename"].is_null()
        || !rns_rete[0]["target"].is_null()
        || rns_rete[0]["uses_default_features"].as_bool() != Some(false)
        || rns_rete[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(format!(
            "reticulum-rns-rete must be one feature-free local dev dependency at {}",
            rns_path.display()
        ));
    }

    let static_cell = development
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some("static_cell"))
        .collect::<Vec<_>>();
    if static_cell.len() != 1
        || static_cell[0]["req"].as_str() != Some("=2.1.1")
        || static_cell[0]["source"].as_str()
            != Some("registry+https://github.com/rust-lang/crates.io-index")
        || !static_cell[0]["path"].is_null()
        || static_cell[0]["optional"].as_bool() != Some(false)
        || !static_cell[0]["rename"].is_null()
        || !static_cell[0]["target"].is_null()
        || static_cell[0]["uses_default_features"].as_bool() != Some(true)
        || static_cell[0]["features"]
            .as_array()
            .is_none_or(|features| !features.is_empty())
    {
        return Err(
            "static_cell must be the unconditional default-featured registry =2.1.1 dev dependency"
                .to_owned(),
        );
    }

    Ok(())
}

fn is_rete_implementation_dependency(name: &str) -> bool {
    name == "reticulum-rns-rete"
        || name.starts_with("reticulum-rns-rete-")
        || name.starts_with("rete-")
}

fn is_platform_implementation_dependency(dependency: &serde_json::Value, workspace: &Path) -> bool {
    let Some(name) = dependency["name"].as_str() else {
        return false;
    };
    if name == "reticulum-heltec-tracker-v2"
        || name.starts_with("reticulum-board-")
        || name.starts_with("reticulum-radio-")
        || name.starts_with("radio-")
        || name.starts_with("lora-")
        || name.starts_with("sx126")
        || name.starts_with("esp-")
        || name.starts_with("esp32")
        || name.starts_with("embassy-")
    {
        return true;
    }

    dependency["path"]
        .as_str()
        .and_then(|path| Path::new(path).strip_prefix(workspace).ok())
        .is_some_and(|relative| {
            let mut components = relative.components();
            match (components.next(), components.next()) {
                (Some(Component::Normal(first)), _) if first == OsStr::new("firmware") => true,
                (Some(Component::Normal(first)), Some(Component::Normal(second)))
                    if first == OsStr::new("crates") =>
                {
                    second.to_str().is_some_and(|name| {
                        name.starts_with("board-") || name.starts_with("radio-")
                    })
                }
                _ => false,
            }
        })
}

fn validate_firmware_dependency_boundary(
    metadata_json: &str,
    workspace: &Path,
) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let matching = packages
        .iter()
        .filter(|package| package["name"].as_str() == Some("reticulum-heltec-tracker-v2"))
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one firmware package, found {}",
            matching.len()
        ));
    }
    let firmware = matching[0];
    let firmware_id = firmware["id"]
        .as_str()
        .ok_or_else(|| "firmware package has no package ID".to_owned())?;
    let dependencies = firmware["dependencies"]
        .as_array()
        .ok_or_else(|| "firmware package has no dependency array".to_owned())?;
    let dependency_names = dependencies
        .iter()
        .filter_map(|dependency| dependency["name"].as_str())
        .collect::<Vec<_>>();

    let resolve_nodes = metadata["resolve"]["nodes"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no resolve nodes".to_owned())?;
    let firmware_nodes = resolve_nodes
        .iter()
        .filter(|node| node["id"].as_str() == Some(firmware_id))
        .collect::<Vec<_>>();
    if firmware_nodes.len() != 1 {
        return Err(format!(
            "expected exactly one resolved firmware node, found {}",
            firmware_nodes.len()
        ));
    }
    let resolved_dependencies = firmware_nodes[0]["deps"]
        .as_array()
        .ok_or_else(|| "resolved firmware node has no dependency list".to_owned())?;

    for (required, relative_path) in [
        (
            "reticulum-board-heltec-tracker-v2",
            "crates/board-heltec-tracker-v2",
        ),
        ("reticulum-radio-interface", "crates/radio-interface"),
        ("reticulum-rns-rete-rx", "crates/rns-rete-rx"),
    ] {
        let declared = dependencies
            .iter()
            .filter(|dependency| dependency["name"].as_str() == Some(required))
            .collect::<Vec<_>>();
        if declared.len() != 1 {
            return Err(format!(
                "firmware must declare exactly one {required} dependency, found {}",
                declared.len()
            ));
        }
        let declared = declared[0];
        if !declared["kind"].is_null()
            || declared["optional"].as_bool() != Some(false)
            || !declared["rename"].is_null()
            || !declared["source"].is_null()
            || !declared["target"].is_null()
        {
            return Err(format!(
                "{required} must be a non-optional, unrenamed, unconditional normal workspace dependency"
            ));
        }
        let expected_manifest = workspace.join(relative_path).join("Cargo.toml");
        let expected_path = workspace.join(relative_path);
        if declared["path"].as_str().map(Path::new) != Some(expected_path.as_path()) {
            return Err(format!(
                "{required} path {:?} does not match {}",
                declared["path"].as_str(),
                expected_path.display()
            ));
        }
        let resolved_packages = packages
            .iter()
            .filter(|package| {
                package["name"].as_str() == Some(required)
                    && package["manifest_path"].as_str().map(Path::new)
                        == Some(expected_manifest.as_path())
                    && package["source"].is_null()
            })
            .collect::<Vec<_>>();
        if resolved_packages.len() != 1 {
            return Err(format!(
                "expected exactly one local resolved {required} package, found {}",
                resolved_packages.len()
            ));
        }
        let required_id = resolved_packages[0]["id"]
            .as_str()
            .ok_or_else(|| format!("resolved {required} package has no package ID"))?;
        let resolved = resolved_dependencies
            .iter()
            .filter(|dependency| dependency["pkg"].as_str() == Some(required_id))
            .collect::<Vec<_>>();
        if resolved.len() != 1
            || resolved[0]["dep_kinds"].as_array().is_none_or(|kinds| {
                kinds.len() != 1 || !kinds[0]["kind"].is_null() || !kinds[0]["target"].is_null()
            })
        {
            return Err(format!(
                "resolved {required} edge is missing or is not one unconditional normal dependency"
            ));
        }
    }
    let hil_name = "reticulum-lab-rx-returned-fault-hil";
    let hil_dependencies = dependencies
        .iter()
        .filter(|dependency| dependency["name"].as_str() == Some(hil_name))
        .collect::<Vec<_>>();
    if hil_dependencies.len() != 1 {
        return Err(format!(
            "firmware must declare exactly one {hil_name} dependency, found {}",
            hil_dependencies.len()
        ));
    }
    let hil_dependency = hil_dependencies[0];
    let hil_path = workspace.join("crates/lab-rx-returned-fault-hil");
    if !hil_dependency["kind"].is_null()
        || hil_dependency["optional"].as_bool() != Some(true)
        || !hil_dependency["rename"].is_null()
        || !hil_dependency["source"].is_null()
        || !hil_dependency["target"].is_null()
        || hil_dependency["path"].as_str().map(Path::new) != Some(hil_path.as_path())
    {
        return Err(format!(
            "{hil_name} must be an optional, unrenamed, unconditional local normal dependency"
        ));
    }
    for forbidden in [
        "lora-phy",
        "rete-core",
        "rete-stack",
        "rete-transport",
        "reticulum-board-heltec-tracker-v2-radio",
        "reticulum-board-heltec-tracker-v2-tx-hil",
        "reticulum-device-api-adapter",
        "reticulum-device-api-credential-store",
        "reticulum-device-api-credentials",
        "reticulum-device-api-framing",
        "reticulum-device-api-pairing-control",
        "reticulum-device-api-pairing",
        "reticulum-device-api-handoff",
        "reticulum-device-api-pairing-policy",
        "reticulum-device-api-session",
        "reticulum-node-core",
        "reticulum-radio-tx-dispatch",
        "reticulum-rns-rete",
        "reticulum-semantic-roundtrip-hil",
        "reticulum-storage-actor",
        "reticulum-storage-journal",
        "reticulum-storage-model",
        "reticulum-submission-projector",
        "reticulum-tx-dispatch",
        "reticulum-tx-handoff",
        "reticulum-tx-supervisor",
    ] {
        if dependency_names.contains(&forbidden) {
            return Err(format!(
                "firmware directly depends on prohibited implementation crate {forbidden}"
            ));
        }
    }

    let package_names = packages
        .iter()
        .filter_map(|package| Some((package["id"].as_str()?, package["name"].as_str()?)))
        .collect::<BTreeMap<_, _>>();
    let resolved_nodes = resolve_nodes
        .iter()
        .filter_map(|node| Some((node["id"].as_str()?, node)))
        .collect::<BTreeMap<_, _>>();
    let mut pending = vec![firmware_id];
    let mut visited = BTreeSet::new();
    while let Some(package_id) = pending.pop() {
        if !visited.insert(package_id) {
            continue;
        }
        if package_id != firmware_id
            && package_names.get(package_id).is_some_and(|name| {
                matches!(
                    *name,
                    "reticulum-board-heltec-tracker-v2-radio"
                        | "reticulum-board-heltec-tracker-v2-tx-hil"
                        | "reticulum-device-api-adapter"
                        | "reticulum-device-api-credential-store"
                        | "reticulum-device-api-credentials"
                        | "reticulum-device-api-framing"
                        | "reticulum-device-api-pairing-control"
                        | "reticulum-device-api-pairing"
                        | "reticulum-device-api-handoff"
                        | "reticulum-device-api-pairing-policy"
                        | "reticulum-device-api-session"
                        | "reticulum-node-core"
                        | "reticulum-radio-tx-dispatch"
                        | "reticulum-semantic-roundtrip-hil"
                        | "reticulum-storage-actor"
                        | "reticulum-storage-journal"
                        | "reticulum-storage-model"
                        | "reticulum-submission-projector"
                        | "reticulum-tx-dispatch"
                        | "reticulum-tx-handoff"
                        | "reticulum-tx-supervisor"
                )
            })
        {
            return Err(format!(
                "firmware resolved graph reaches prohibited pre-integration package {}",
                package_names[package_id]
            ));
        }
        let node = resolved_nodes
            .get(package_id)
            .ok_or_else(|| format!("reachable package {package_id} has no resolved node"))?;
        let node_dependencies = node["deps"]
            .as_array()
            .ok_or_else(|| format!("resolved package {package_id} has no dependency list"))?;
        pending.extend(
            node_dependencies
                .iter()
                .filter_map(|dependency| dependency["pkg"].as_str()),
        );
    }
    Ok(())
}

const RX_API_SNAPSHOT: &str = "docs/api/receive-only-surface.txt";

fn rx_api_policy() -> ExitCode {
    let root = workspace_root();
    let actual = match collect_receive_only_surface(&root) {
        Ok(surface) => format!("{}\n", surface.join("\n")),
        Err(error) => {
            eprintln!("error: could not inspect receive-only API surface: {error}");
            return ExitCode::FAILURE;
        }
    };
    let snapshot_path = root.join(RX_API_SNAPSHOT);
    let expected = match fs::read_to_string(&snapshot_path) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!(
                "error: could not read receive-only API snapshot {}: {error}",
                snapshot_path.display()
            );
            eprintln!("generated surface follows:\n{actual}");
            return ExitCode::FAILURE;
        }
    };

    if expected == actual {
        match validate_external_receive_only_contract(&root) {
            Ok(()) => {
                println!(
                    "ok: receive-only board/façade API matches the reviewed source snapshot and external compile contract"
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("error: receive-only external API contract failed: {error}");
                ExitCode::FAILURE
            }
        }
    } else {
        eprintln!(
            "error: receive-only public API differs from {}; review every change before updating it",
            snapshot_path.display()
        );
        eprintln!("generated surface follows:\n{actual}");
        ExitCode::FAILURE
    }
}

fn validate_external_receive_only_contract(workspace: &Path) -> Result<(), String> {
    let contract =
        env::temp_dir().join(format!("reticulum-rx-api-contract-{}", std::process::id()));
    if contract.exists() {
        fs::remove_dir_all(&contract)
            .map_err(|error| format!("could not clear {}: {error}", contract.display()))?;
    }
    let bins = contract.join("src/bin");
    fs::create_dir_all(&bins)
        .map_err(|error| format!("could not create {}: {error}", bins.display()))?;

    let manifest = format!(
        r#"[workspace]

[package]
name = "reticulum-rx-api-contract"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
embedded-hal = "=1.0.0"
embedded-hal-async = "=1.0.0"
reticulum-board-heltec-tracker-v2 = {{ path = {:?} }}
reticulum-rns-rete-rx = {{ path = {:?} }}
"#,
        workspace.join("crates/board-heltec-tracker-v2"),
        workspace.join("crates/rns-rete-rx"),
    );
    fs::write(contract.join("Cargo.toml"), manifest)
        .map_err(|error| format!("could not write external contract manifest: {error}"))?;

    let approved = r#"
use core::num::NonZeroU64;
use reticulum_rns_rete_rx::{
    ReceiveOnlyInterfaceId, ReceiveOnlyRete, receive_only_identity_from_private_key,
};

fn main() {
    let identity = receive_only_identity_from_private_key(&[0x42; 64]).unwrap();
    let _ = identity.public_key();
    let owner = ReceiveOnlyRete::<16, 4, 32, 2>::new(
        identity,
        "external-contract",
        &["receive-only"],
        NonZeroU64::new(100).unwrap(),
        0,
        NonZeroU64::new(5).unwrap(),
        ReceiveOnlyInterfaceId(1),
    )
    .unwrap();
    let _ = owner.destination_hash();
    let _ = owner.identity_hash();
    let _ = owner.fragment_deadline_ticks();
    let _ = owner.next_wake_ticks();
    let _ = owner.metrics();
}
"#;
    let radio_method_prefix = r#"
use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use embedded_hal_async::{delay::DelayNs, digital::Wait, spi::SpiDevice};
use reticulum_board_heltec_tracker_v2::TrackerRxRadio;

fn forbidden<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx>(
    radio: &mut TrackerRxRadio<Spi, Reset, Dio1, Busy, RadioDelay, Power, Csd, Ctx>,
)
where
    Spi: SpiDevice<u8>,
    Reset: OutputPin,
    Dio1: Wait,
    Busy: Wait,
    RadioDelay: DelayNs,
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
{
"#;
    let cases = [
        ("approved", approved.to_owned(), None),
        (
            "board_dependency_escape",
            "use reticulum_board_heltec_tracker_v2::lora_phy::LoRa; fn main() {}\n".to_owned(),
            Some("could not find `lora_phy`"),
        ),
        (
            "board_direct_type_escape",
            "use reticulum_board_heltec_tracker_v2::LoRa; fn main() {}\n".to_owned(),
            Some("no `LoRa` in the root"),
        ),
        (
            "rete_dependency_escape",
            "use reticulum_rns_rete_rx::reticulum_rns_rete::EmbeddedNode; fn main() {}\n"
                .to_owned(),
            Some("could not find `reticulum_rns_rete`"),
        ),
        (
            "rete_direct_type_escape",
            "use reticulum_rns_rete_rx::EmbeddedNode; fn main() {}\n".to_owned(),
            Some("no `EmbeddedNode` in the root"),
        ),
        (
            "rete_send_escape",
            r#"use reticulum_rns_rete_rx::ReceiveOnlyRete;
fn forbidden(owner: &mut ReceiveOnlyRete<16, 4, 32, 2>) { owner.send_data(); }
fn main() {}
"#
            .to_owned(),
            Some("no method named `send_data`"),
        ),
        (
            "rete_inner_escape",
            r#"use reticulum_rns_rete_rx::ReceiveOnlyRete;
fn forbidden(owner: &mut ReceiveOnlyRete<16, 4, 32, 2>) { owner.inner(); }
fn main() {}
"#
            .to_owned(),
            Some("no method named `inner`"),
        ),
        (
            "rete_link_escape",
            r#"use reticulum_rns_rete_rx::ReceiveOnlyRete;
fn forbidden(owner: &mut ReceiveOnlyRete<16, 4, 32, 2>) { owner.initiate_link(); }
fn main() {}
"#
            .to_owned(),
            Some("no method named `initiate_link`"),
        ),
        (
            "radio_tx_escape",
            format!("{radio_method_prefix}    radio.tx();\n}}\nfn main() {{}}\n"),
            Some("no method named `tx`"),
        ),
        (
            "radio_inner_escape",
            format!("{radio_method_prefix}    radio.into_inner();\n}}\nfn main() {{}}\n"),
            Some("no method named `into_inner`"),
        ),
        (
            "radio_prepare_tx_escape",
            format!("{radio_method_prefix}    radio.prepare_for_tx();\n}}\nfn main() {{}}\n"),
            Some("no method named `prepare_for_tx`"),
        ),
        (
            "radio_continuous_wave_escape",
            format!("{radio_method_prefix}    radio.continuous_wave();\n}}\nfn main() {{}}\n"),
            Some("no method named `continuous_wave`"),
        ),
        (
            "interlock_enable_escape",
            r#"
use embedded_hal::digital::{OutputPin, StatefulOutputPin};
use embedded_hal_async::delay::DelayNs;
use reticulum_board_heltec_tracker_v2::TrackerRxInterlock;

fn forbidden<Power, Csd, Ctx, D>(
    owner: TrackerRxInterlock<Power, Csd, Ctx>,
    delay: &mut D,
)
where
    Power: OutputPin,
    Csd: OutputPin,
    Ctx: StatefulOutputPin,
    D: DelayNs,
{
    let _ = owner.enable(delay);
}
fn main() {}
"#
            .to_owned(),
            Some("method `enable` is private"),
        ),
    ];

    for (name, source, expected_failure) in cases {
        fs::write(bins.join(format!("{name}.rs")), source)
            .map_err(|error| format!("could not write external contract case {name}: {error}"))?;
        let output = Command::new("cargo")
            .current_dir(&contract)
            .args(["check", "--offline", "--quiet", "--bin", name])
            .output()
            .map_err(|error| format!("could not compile external contract case {name}: {error}"))?;
        let diagnostics = String::from_utf8_lossy(&output.stderr);
        match expected_failure {
            None if !output.status.success() => {
                return Err(format!(
                    "approved external surface did not compile:\n{diagnostics}"
                ));
            }
            Some(_) if output.status.success() => {
                return Err(format!(
                    "prohibited external case {name} unexpectedly compiled"
                ));
            }
            Some(expected) if !diagnostics.contains(expected) => {
                return Err(format!(
                    "prohibited external case {name} failed for the wrong reason; expected {expected:?}:\n{diagnostics}"
                ));
            }
            None | Some(_) => {}
        }
    }

    fs::remove_dir_all(&contract)
        .map_err(|error| format!("could not remove {}: {error}", contract.display()))?;
    Ok(())
}

fn print_rx_api_surface() -> ExitCode {
    match collect_receive_only_surface(&workspace_root()) {
        Ok(surface) => {
            println!("{}", surface.join("\n"));
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: could not inspect receive-only API surface: {error}");
            ExitCode::FAILURE
        }
    }
}

fn collect_receive_only_surface(root: &Path) -> Result<Vec<String>, String> {
    let mut surface = Vec::new();
    collect_crate_source_api(root, "crates/rns-rete-rx", "rete-rx", &mut surface)?;
    collect_crate_source_api(
        root,
        "crates/board-heltec-tracker-v2",
        "board",
        &mut surface,
    )?;

    surface.sort();
    surface.dedup();
    Ok(surface)
}

fn collect_crate_source_api(
    workspace: &Path,
    crate_path: &str,
    prefix: &str,
    surface: &mut Vec<String>,
) -> Result<(), String> {
    let crate_root = workspace.join(crate_path);
    let mut sources = Vec::new();
    collect_rust_sources(&crate_root.join("src"), &mut sources)?;
    sources.sort();
    let mut parsed_sources = Vec::new();
    for path in sources {
        let relative = path
            .strip_prefix(&crate_root)
            .map_err(|error| format!("could not relativize {}: {error}", path.display()))?;
        let file = parse_rust_file(&path)?;
        parsed_sources.push((format!("{prefix}@{}", relative.display()), file));
    }

    // Implementations can legally live in any file or private module in the
    // defining crate. Gather candidate public type names across the complete
    // crate before inspecting a single impl, so an extension file cannot add a
    // public method or trait escape without changing the snapshot.
    let mut public_types = BTreeSet::new();
    for (_, file) in &parsed_sources {
        collect_public_type_names(&file.items, &mut public_types);
    }
    for (source_prefix, file) in parsed_sources {
        collect_public_file_api_with_types(&source_prefix, &file, &public_types, surface);
    }
    Ok(())
}

fn collect_rust_sources(directory: &Path, sources: &mut Vec<PathBuf>) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not enumerate {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "could not inspect an entry below {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        if file_type.is_dir() {
            collect_rust_sources(&path, sources)?;
        } else if file_type.is_file() && path.extension() == Some(OsStr::new("rs")) {
            sources.push(path);
        }
    }
    Ok(())
}

fn parse_rust_file(path: &Path) -> Result<syn::File, String> {
    let source = fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    syn::parse_file(&source).map_err(|error| format!("could not parse {}: {error}", path.display()))
}

#[cfg(test)]
fn collect_public_file_api(prefix: &str, file: &syn::File, surface: &mut Vec<String>) {
    let mut public_types = BTreeSet::new();
    collect_public_type_names(&file.items, &mut public_types);
    collect_public_file_api_with_types(prefix, file, &public_types, surface);
}

fn collect_public_file_api_with_types(
    prefix: &str,
    file: &syn::File,
    public_types: &BTreeSet<String>,
    surface: &mut Vec<String>,
) {
    collect_public_items(prefix, &file.items, public_types, surface);
}

fn collect_public_type_names(items: &[Item], public_types: &mut BTreeSet<String>) {
    for item in items {
        match item {
            Item::Struct(item) if is_public(&item.vis) => {
                public_types.insert(item.ident.to_string());
            }
            Item::Enum(item) if is_public(&item.vis) => {
                public_types.insert(item.ident.to_string());
            }
            Item::Type(item) if is_public(&item.vis) => {
                public_types.insert(item.ident.to_string());
            }
            Item::Union(item) if is_public(&item.vis) => {
                public_types.insert(item.ident.to_string());
            }
            Item::Mod(item) => {
                if let Some((_, nested)) = &item.content {
                    collect_public_type_names(nested, public_types);
                }
            }
            _ => {}
        }
    }
}

fn collect_public_items(
    prefix: &str,
    items: &[Item],
    public_types: &BTreeSet<String>,
    surface: &mut Vec<String>,
) {
    for item in items {
        match item {
            Item::Use(item) if is_public(&item.vis) => {
                collect_api_attributes(prefix, "reexport", &item.attrs, surface);
                surface.push(format!("{prefix} reexport {}", item.tree.to_token_stream()));
            }
            Item::ExternCrate(item) if is_public(&item.vis) => {
                collect_api_attributes(prefix, "extern-crate", &item.attrs, surface);
                let rename = item
                    .rename
                    .as_ref()
                    .map_or_else(String::new, |(_, ident)| format!(" as {ident}"));
                surface.push(format!("{prefix} extern crate {}{rename}", item.ident));
            }
            Item::Mod(item) => {
                if is_public(&item.vis) {
                    collect_api_attributes(
                        prefix,
                        &format!("mod {}", item.ident),
                        &item.attrs,
                        surface,
                    );
                    surface.push(format!("{prefix} mod {}", item.ident));
                }
                if let Some((_, nested)) = &item.content {
                    collect_public_items(
                        &format!("{prefix}::{}", item.ident),
                        nested,
                        public_types,
                        surface,
                    );
                }
            }
            Item::Struct(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("struct {}", item.ident),
                    &item.attrs,
                    surface,
                );
                collect_struct(
                    prefix,
                    &item.ident.to_string(),
                    &item.generics,
                    &item.fields,
                    surface,
                );
            }
            Item::Enum(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("enum {}", item.ident),
                    &item.attrs,
                    surface,
                );
                surface.push(format!(
                    "{prefix} enum {}{}",
                    item.ident,
                    item.generics.to_token_stream()
                ));
                for variant in &item.variants {
                    surface.push(format!(
                        "{prefix} enum {} variant {}",
                        item.ident,
                        variant_signature(variant)
                    ));
                }
            }
            Item::Union(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("union {}", item.ident),
                    &item.attrs,
                    surface,
                );
                collect_struct(
                    prefix,
                    &item.ident.to_string(),
                    &item.generics,
                    &Fields::Named(item.fields.clone()),
                    surface,
                );
            }
            Item::Fn(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("fn {}", item.sig.ident),
                    &item.attrs,
                    surface,
                );
                surface.push(format!("{prefix} {}", item.sig.to_token_stream()));
            }
            Item::Const(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("const {}", item.ident),
                    &item.attrs,
                    surface,
                );
                surface.push(format!(
                    "{prefix} const {} : {}",
                    item.ident,
                    item.ty.to_token_stream()
                ));
            }
            Item::Type(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("type {}", item.ident),
                    &item.attrs,
                    surface,
                );
                surface.push(format!(
                    "{prefix} type {}{} = {}",
                    item.ident,
                    item.generics.to_token_stream(),
                    item.ty.to_token_stream()
                ));
            }
            Item::Static(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("static {}", item.ident),
                    &item.attrs,
                    surface,
                );
                surface.push(format!(
                    "{prefix} static {} {} : {}",
                    item.mutability.to_token_stream(),
                    item.ident,
                    item.ty.to_token_stream()
                ));
            }
            Item::Trait(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("trait {}", item.ident),
                    &item.attrs,
                    surface,
                );
                surface.push(format!(
                    "{prefix} trait {}{} : {}",
                    item.ident,
                    item.generics.to_token_stream(),
                    item.supertraits.to_token_stream()
                ));
                for trait_item in &item.items {
                    surface.push(format!(
                        "{prefix} trait {} item {}",
                        item.ident,
                        trait_item.to_token_stream()
                    ));
                }
            }
            Item::TraitAlias(item) if is_public(&item.vis) => {
                collect_api_attributes(
                    prefix,
                    &format!("trait-alias {}", item.ident),
                    &item.attrs,
                    surface,
                );
                surface.push(format!(
                    "{prefix} trait-alias {}{} = {}",
                    item.ident,
                    item.generics.to_token_stream(),
                    item.bounds.to_token_stream()
                ));
            }
            Item::Macro(item) => {
                let name = item
                    .ident
                    .as_ref()
                    .map_or_else(|| "<anonymous>".to_owned(), ToString::to_string);
                collect_api_attributes(prefix, &format!("item-macro {name}"), &item.attrs, surface);
                surface.push(format!(
                    "{prefix} item-macro {name} {} {}",
                    item.mac.path.to_token_stream(),
                    item.mac.tokens
                ));
                if item
                    .attrs
                    .iter()
                    .any(|attribute| attribute.path().is_ident("macro_export"))
                {
                    surface.push(format!("{prefix} exported-macro {name}"));
                }
            }
            Item::Impl(item) => {
                if let Some(self_type) = public_self_type(&item.self_ty, public_types) {
                    collect_api_attributes(
                        prefix,
                        &format!("impl {self_type}"),
                        &item.attrs,
                        surface,
                    );
                    collect_impl(prefix, &self_type, item, surface);
                }
            }
            _ => {}
        }
    }
}

fn collect_api_attributes(
    prefix: &str,
    owner: &str,
    attributes: &[syn::Attribute],
    surface: &mut Vec<String>,
) {
    for attribute in attributes {
        // Documentation does not alter the callable boundary. Every other
        // local attribute is retained because derive and attribute macros can
        // synthesize methods or trait implementations before type checking.
        if !attribute.path().is_ident("doc") {
            surface.push(format!(
                "{prefix} {owner} attribute {}",
                attribute.to_token_stream()
            ));
        }
    }
}

fn collect_struct(
    prefix: &str,
    name: &str,
    generics: &syn::Generics,
    fields: &Fields,
    surface: &mut Vec<String>,
) {
    let all_private = fields.iter().all(|field| !is_public(&field.vis));
    surface.push(format!(
        "{prefix} struct {name}{} fields={}",
        generics.to_token_stream(),
        if all_private { "opaque" } else { "exposed" }
    ));
    if !all_private {
        for (index, field) in fields.iter().enumerate() {
            let field_name = field
                .ident
                .as_ref()
                .map_or_else(|| index.to_string(), ToString::to_string);
            let visibility = if is_public(&field.vis) {
                "pub"
            } else {
                "private"
            };
            surface.push(format!(
                "{prefix} struct {name} field {field_name} {visibility} {}",
                field.ty.to_token_stream()
            ));
        }
    }
}

fn collect_impl(prefix: &str, self_type: &str, item: &syn::ItemImpl, surface: &mut Vec<String>) {
    if let Some((_, trait_path, _)) = &item.trait_ {
        surface.push(format!(
            "{prefix} impl {} for {self_type}",
            trait_path.to_token_stream()
        ));
    }
    for impl_item in &item.items {
        match impl_item {
            ImplItem::Fn(function) if item.trait_.is_some() || is_public(&function.vis) => {
                surface.push(format!(
                    "{prefix} impl {self_type} {}",
                    function.sig.to_token_stream()
                ));
            }
            ImplItem::Const(constant) if item.trait_.is_some() || is_public(&constant.vis) => {
                surface.push(format!(
                    "{prefix} impl {self_type} const {} : {}",
                    constant.ident,
                    constant.ty.to_token_stream()
                ));
            }
            ImplItem::Type(associated) if item.trait_.is_some() || is_public(&associated.vis) => {
                surface.push(format!(
                    "{prefix} impl {self_type} type {} = {}",
                    associated.ident,
                    associated.ty.to_token_stream()
                ));
            }
            _ => {}
        }
    }
}

fn variant_signature(variant: &syn::Variant) -> String {
    let fields = variant.fields.to_token_stream().to_string();
    let mut signature = variant.ident.to_string();
    if !fields.is_empty() {
        signature.push(' ');
        signature.push_str(&fields);
    }
    if let Some((_, discriminant)) = &variant.discriminant {
        signature.push_str(" = ");
        signature.push_str(&discriminant.to_token_stream().to_string());
    }
    signature
}

fn public_self_type(ty: &Type, public_types: &BTreeSet<String>) -> Option<String> {
    let rendered = ty.to_token_stream().to_string();
    let mentions_public_type = rendered
        .split(|character: char| !(character.is_alphanumeric() || character == '_'))
        .any(|token| public_types.contains(token));
    mentions_public_type.then_some(rendered)
}

fn is_public(visibility: &Visibility) -> bool {
    matches!(visibility, Visibility::Public(_))
}

fn validate_resolved_rete_pin(
    metadata_json: &str,
    reported_source: &str,
    reported_revision: &str,
) -> Result<(), String> {
    if reported_revision.len() != 40
        || !reported_revision
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(format!(
            "reported revision {:?} is not a full Git object ID",
            reported_revision
        ));
    }

    let repository = reported_source
        .strip_suffix(".git")
        .unwrap_or(reported_source);
    let expected_source = format!(
        "git+{repository}.git?rev={revision}#{revision}",
        revision = reported_revision
    );
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;

    for name in ["rete-core", "rete-stack", "rete-transport"] {
        let matching = packages
            .iter()
            .filter(|package| package["name"].as_str() == Some(name))
            .collect::<Vec<_>>();
        if matching.len() != 1 {
            return Err(format!(
                "expected exactly one resolved {name} package, found {}",
                matching.len()
            ));
        }
        let source = matching[0]["source"]
            .as_str()
            .ok_or_else(|| format!("resolved {name} package has no Git source"))?;
        if source != expected_source {
            return Err(format!(
                "resolved {name} source {source:?} does not match report-derived {expected_source:?}"
            ));
        }
    }

    Ok(())
}

fn validate_resolved_esp_rtos_patch(metadata_json: &str, workspace: &Path) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let matching = packages
        .iter()
        .filter(|package| package["name"].as_str() == Some("esp-rtos"))
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one resolved esp-rtos package, found {}",
            matching.len()
        ));
    }

    let package = matching[0];
    if package["version"].as_str() != Some("0.3.0") {
        return Err(format!(
            "resolved esp-rtos version {:?} is not the reviewed 0.3.0 base",
            package["version"].as_str()
        ));
    }
    if !package["source"].is_null() {
        return Err(format!(
            "resolved esp-rtos source {:?} is not the reviewed local path",
            package["source"].as_str()
        ));
    }
    let expected_manifest = workspace.join("vendor/esp-rtos-0.3.0/Cargo.toml");
    if package["manifest_path"].as_str().map(Path::new) != Some(expected_manifest.as_path()) {
        return Err(format!(
            "resolved esp-rtos manifest {:?} does not match {}",
            package["manifest_path"].as_str(),
            expected_manifest.display()
        ));
    }

    validate_esp_rtos_vendor_tree(&workspace.join("vendor/esp-rtos-0.3.0"))
}

fn validate_resolved_lora_phy_patch(metadata_json: &str, workspace: &Path) -> Result<(), String> {
    let metadata: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|error| format!("could not parse cargo metadata: {error}"))?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or_else(|| "cargo metadata has no packages array".to_owned())?;
    let matching = packages
        .iter()
        .filter(|package| package["name"].as_str() == Some("lora-phy"))
        .collect::<Vec<_>>();
    if matching.len() != 1 {
        return Err(format!(
            "expected exactly one resolved lora-phy package, found {}",
            matching.len()
        ));
    }

    let package = matching[0];
    if package["version"].as_str() != Some("3.0.1") {
        return Err(format!(
            "resolved lora-phy version {:?} is not the reviewed 3.0.1 base",
            package["version"].as_str()
        ));
    }
    if !package["source"].is_null() {
        return Err(format!(
            "resolved lora-phy source {:?} is not the reviewed local path",
            package["source"].as_str()
        ));
    }
    let expected_manifest = workspace.join("vendor/lora-phy-3.0.1/Cargo.toml");
    if package["manifest_path"].as_str().map(Path::new) != Some(expected_manifest.as_path()) {
        return Err(format!(
            "resolved lora-phy manifest {:?} does not match {}",
            package["manifest_path"].as_str(),
            expected_manifest.display()
        ));
    }

    validate_lora_phy_vendor_tree(&workspace.join("vendor/lora-phy-3.0.1"))
}

const ESP_RTOS_VENDOR_MANIFEST: &str = "VENDOR-HASHES.json";
const ESP_RTOS_ARCHIVE_SHA256: &str =
    "551f90766e1527edaa0c91e8d559e9e2a60397b545e93357ac61fb31845e5712";
const ESP_RTOS_UPSTREAM_COMMIT: &str = "347003de8a48320bb7724f53045be3afa9204411";
const ESP_RTOS_PRISTINE_LIB_SHA256: &str =
    "0de5aec7bf732bba96fe6c1218fc634a5e72c9daed26c5bdbde726d7ebd0d0f9";
const ESP_RTOS_CPU0_UPSTREAM_STACK_LENGTH: &str =
    "            stack_top as usize - stack_bottom as usize,";
const ESP_RTOS_CPU0_PATCHED_STACK_LENGTH: &str = "            (stack_top as usize - stack_bottom as usize)\n                / core::mem::size_of::<MaybeUninit<u32>>(),";
const ESP_RTOS_CPU1_UPSTREAM_STACK_LENGTH: &str = "            STACK_SIZE,";
const ESP_RTOS_CPU1_PATCHED_STACK_LENGTH: &str =
    "            STACK_SIZE / core::mem::size_of::<MaybeUninit<u32>>(),";

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VendorHashManifest {
    schema: u32,
    crate_name: String,
    crate_version: String,
    archive_sha256: String,
    upstream_commit: String,
    unmodified_upstream_files: BTreeMap<String, String>,
    patched_upstream_files: BTreeMap<String, PatchedUpstreamFile>,
    omitted_upstream_files: BTreeMap<String, String>,
    project_files: BTreeMap<String, String>,
    reviewed_source_edits: Vec<ReviewedSourceEdit>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PatchedUpstreamFile {
    upstream_sha256: String,
    vendored_sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewedSourceEdit {
    path: String,
    upstream: String,
    vendored: String,
}

fn validate_esp_rtos_vendor_tree(vendor: &Path) -> Result<(), String> {
    let manifest_path = vendor.join(ESP_RTOS_VENDOR_MANIFEST);
    let text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "could not read checked vendor manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: VendorHashManifest = serde_json::from_str(&text).map_err(|error| {
        format!(
            "could not parse checked vendor manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    validate_esp_rtos_vendor_tree_with_manifest(vendor, &manifest)
}

fn validate_esp_rtos_vendor_tree_with_manifest(
    vendor: &Path,
    manifest: &VendorHashManifest,
) -> Result<(), String> {
    if manifest.schema != 1
        || manifest.crate_name != "esp-rtos"
        || manifest.crate_version != "0.3.0"
        || manifest.archive_sha256 != ESP_RTOS_ARCHIVE_SHA256
        || manifest.upstream_commit != ESP_RTOS_UPSTREAM_COMMIT
    {
        return Err(
            "checked vendor manifest does not identify the reviewed esp-rtos 0.3.0 archive"
                .to_owned(),
        );
    }

    if manifest.omitted_upstream_files.len() != 1
        || manifest
            .omitted_upstream_files
            .get("Cargo.lock")
            .map(String::as_str)
            != Some("72ab2b50ff8cbed99f8f2f8b85963e5e3561a87ec56a1538a18c209245009d0f")
    {
        return Err(
            "checked vendor manifest must omit exactly the published package-local Cargo.lock"
                .to_owned(),
        );
    }
    let expected_edits = [
        (
            ESP_RTOS_CPU0_UPSTREAM_STACK_LENGTH,
            ESP_RTOS_CPU0_PATCHED_STACK_LENGTH,
        ),
        (
            ESP_RTOS_CPU1_UPSTREAM_STACK_LENGTH,
            ESP_RTOS_CPU1_PATCHED_STACK_LENGTH,
        ),
    ];
    if manifest.reviewed_source_edits.len() != expected_edits.len()
        || manifest
            .reviewed_source_edits
            .iter()
            .zip(expected_edits)
            .any(|(edit, (upstream, vendored))| {
                edit.path != "src/lib.rs" || edit.upstream != upstream || edit.vendored != vendored
            })
    {
        return Err(
            "checked vendor manifest must describe the exact reviewed CPU0 and CPU1 src/lib.rs edits"
                .to_owned(),
        );
    }
    let patched_lib = manifest
        .patched_upstream_files
        .get("src/lib.rs")
        .ok_or_else(|| "checked vendor manifest does not identify patched src/lib.rs".to_owned())?;
    if manifest.patched_upstream_files.len() != 1
        || patched_lib.upstream_sha256 != ESP_RTOS_PRISTINE_LIB_SHA256
    {
        return Err(
            "checked vendor manifest does not bind the sole patched file to pristine src/lib.rs"
                .to_owned(),
        );
    }

    let mut expected_files = BTreeMap::new();
    for (role, files) in [
        ("unmodified upstream", &manifest.unmodified_upstream_files),
        ("project provenance", &manifest.project_files),
    ] {
        for (relative, digest) in files {
            validate_vendor_relative_path(relative)?;
            validate_sha256_digest(digest)?;
            if expected_files
                .insert(relative.clone(), digest.clone())
                .is_some()
            {
                return Err(format!(
                    "checked vendor manifest lists {relative:?} in more than one role ({role})"
                ));
            }
        }
    }
    for (relative, record) in &manifest.patched_upstream_files {
        validate_vendor_relative_path(relative)?;
        validate_sha256_digest(&record.upstream_sha256)?;
        validate_sha256_digest(&record.vendored_sha256)?;
        if expected_files
            .insert(relative.clone(), record.vendored_sha256.clone())
            .is_some()
        {
            return Err(format!(
                "checked vendor manifest lists patched file {relative:?} in more than one role"
            ));
        }
    }
    for (relative, digest) in &manifest.omitted_upstream_files {
        validate_vendor_relative_path(relative)?;
        validate_sha256_digest(digest)?;
        if vendor.join(relative).exists() {
            return Err(format!(
                "intentionally omitted upstream file {relative:?} is present in the vendor tree"
            ));
        }
    }
    if expected_files
        .insert(ESP_RTOS_VENDOR_MANIFEST.to_owned(), String::new())
        .is_some()
    {
        return Err(format!(
            "checked vendor manifest must not classify {ESP_RTOS_VENDOR_MANIFEST} as payload"
        ));
    }

    let mut actual_files = BTreeSet::new();
    collect_vendor_files(vendor, vendor, &mut actual_files)?;
    let expected_paths = expected_files.keys().cloned().collect::<BTreeSet<_>>();
    if actual_files != expected_paths {
        let missing = expected_paths
            .difference(&actual_files)
            .cloned()
            .collect::<Vec<_>>();
        let unexpected = actual_files
            .difference(&expected_paths)
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "vendor tree differs from checked inventory; missing={missing:?}, unexpected={unexpected:?}"
        ));
    }

    for (relative, expected_digest) in expected_files {
        if relative == ESP_RTOS_VENDOR_MANIFEST {
            continue;
        }
        let actual_digest = sha256_file(&vendor.join(&relative))?;
        if actual_digest != expected_digest {
            return Err(format!(
                "vendor file {relative:?} digest {actual_digest} does not match checked {expected_digest}"
            ));
        }
    }

    let mut reconstructed = fs::read_to_string(vendor.join("src/lib.rs"))
        .map_err(|error| format!("could not read patched src/lib.rs: {error}"))?;
    for edit in &manifest.reviewed_source_edits {
        let vendored_occurrences = reconstructed.matches(&edit.vendored).count();
        let upstream_occurrences = reconstructed.matches(&edit.upstream).count();
        if vendored_occurrences != 1 || upstream_occurrences != 0 {
            return Err(format!(
                "reviewed edit in {:?} has vendored occurrences {vendored_occurrences} and upstream occurrences {upstream_occurrences}, expected 1 and 0",
                edit.path
            ));
        }
        reconstructed = reconstructed.replacen(&edit.vendored, &edit.upstream, 1);
    }
    let reconstructed_digest = sha256_bytes(reconstructed.as_bytes());
    if reconstructed_digest != patched_lib.upstream_sha256 {
        return Err(format!(
            "reversing the two reviewed edits produced src/lib.rs digest {reconstructed_digest}, expected pristine {}",
            patched_lib.upstream_sha256
        ));
    }

    Ok(())
}

const LORA_PHY_VENDOR_MANIFEST: &str = "VENDOR-HASHES.json";
const LORA_PHY_ARCHIVE_SHA256: &str =
    "61471c3b2909789e3332083577f6cf6c41a4fcf37674ef15156bcbb20504ac65";
const LORA_PHY_UPSTREAM_COMMIT: &str = "ca04c2284eb00e015528933ea5159cd1ff36142d";
const LORA_PHY_PATCHES_SHA256: &str =
    "6cf20617bf00597361b75cc97e14e0debe5022a048bff60a490397486c614258";
const LORA_PHY_UNMODIFIED_UPSTREAM_FILES: [&str; 17] = [
    ".cargo_vcs_info.json",
    "CHANGELOG.md",
    "Cargo.toml",
    "Cargo.toml.orig",
    "LICENSE-APACHE",
    "LICENSE-MIT",
    "README.md",
    "rustfmt.toml",
    "src/interface.rs",
    "src/iv.rs",
    "src/lorawan_radio.rs",
    "src/mod_params.rs",
    "src/sx126x/radio_kind_params.rs",
    "src/sx127x/mod.rs",
    "src/sx127x/radio_kind_params.rs",
    "src/sx127x/sx1272.rs",
    "src/sx127x/sx1276.rs",
];
const LORA_PHY_PATCHED_UPSTREAM_FILES: [(&str, &str, &str); 4] = [
    (
        "src/lib.rs",
        "8df0ef81a3a6333a7f528f0bd44204c65d2614ddf5e92a86c1db61bf25f6eccf",
        "6b936e8546004f87e6003488fd793ad982cda85eec65073c453f412af41824ea",
    ),
    (
        "src/mod_traits.rs",
        "b95e71ba7a7591364a59ddf1961620f8864103a0991df28d71a07026541f6efd",
        "78aee410464aa0e85112ddc3fd790456ee38cf9f289757cf6aac783d94e3f618",
    ),
    (
        "src/sx126x/mod.rs",
        "a1f49190dbb1e5820993bb9e9c1f45481997f047be983017aa2b3c57629faf18",
        "a11be17de1603ebc796070a2d4a6f30dc23c63a305bc2111843d846d16550c3f",
    ),
    (
        "src/sx126x/variant.rs",
        "6ba1ab372039da00dbad8096b40fbf614cb8a2cd443783ede8a95eb9565a657a",
        "daef22e7907e6502e9cb6622e3893fbc492389dcc3b856d88098712aa7262f63",
    ),
];
const LORA_PHY_REVIEWED_EDIT_PATHS: [&str; 16] = [
    "src/sx126x/mod.rs",
    "src/sx126x/mod.rs",
    "src/sx126x/mod.rs",
    "src/sx126x/mod.rs",
    "src/sx126x/mod.rs",
    "src/sx126x/variant.rs",
    "src/sx126x/variant.rs",
    "src/mod_traits.rs",
    "src/mod_traits.rs",
    "src/lib.rs",
    "src/sx126x/mod.rs",
    "src/lib.rs",
    "src/mod_traits.rs",
    "src/mod_traits.rs",
    "src/lib.rs",
    "src/sx126x/mod.rs",
];

fn validate_lora_phy_vendor_tree(vendor: &Path) -> Result<(), String> {
    let manifest_path = vendor.join(LORA_PHY_VENDOR_MANIFEST);
    let text = fs::read_to_string(&manifest_path).map_err(|error| {
        format!(
            "could not read checked lora-phy vendor manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let manifest: VendorHashManifest = serde_json::from_str(&text).map_err(|error| {
        format!(
            "could not parse checked lora-phy vendor manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    validate_lora_phy_vendor_tree_with_manifest(vendor, &manifest)
}

fn validate_lora_phy_vendor_tree_with_manifest(
    vendor: &Path,
    manifest: &VendorHashManifest,
) -> Result<(), String> {
    if manifest.schema != 1
        || manifest.crate_name != "lora-phy"
        || manifest.crate_version != "3.0.1"
        || manifest.archive_sha256 != LORA_PHY_ARCHIVE_SHA256
        || manifest.upstream_commit != LORA_PHY_UPSTREAM_COMMIT
    {
        return Err(
            "checked vendor manifest does not identify the reviewed lora-phy 3.0.1 archive"
                .to_owned(),
        );
    }

    if !manifest.omitted_upstream_files.is_empty() {
        return Err("the published lora-phy 3.0.1 archive has no omitted files".to_owned());
    }
    if manifest.project_files.len() != 1
        || manifest.project_files.get("PATCHES.md").map(String::as_str)
            != Some(LORA_PHY_PATCHES_SHA256)
    {
        return Err(
            "checked lora-phy vendor manifest must contain only the reviewed PATCHES.md".to_owned(),
        );
    }

    let unmodified_paths = manifest
        .unmodified_upstream_files
        .keys()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let expected_unmodified_paths = LORA_PHY_UNMODIFIED_UPSTREAM_FILES
        .into_iter()
        .collect::<BTreeSet<_>>();
    if unmodified_paths != expected_unmodified_paths {
        return Err("checked lora-phy manifest has the wrong pristine file inventory".to_owned());
    }

    if manifest.patched_upstream_files.len() != LORA_PHY_PATCHED_UPSTREAM_FILES.len() {
        return Err(
            "checked lora-phy manifest must identify exactly four patched files".to_owned(),
        );
    }
    for (relative, upstream_sha256, vendored_sha256) in LORA_PHY_PATCHED_UPSTREAM_FILES {
        let record = manifest
            .patched_upstream_files
            .get(relative)
            .ok_or_else(|| format!("checked lora-phy manifest is missing patched {relative}"))?;
        if record.upstream_sha256 != upstream_sha256 || record.vendored_sha256 != vendored_sha256 {
            return Err(format!(
                "checked lora-phy manifest does not bind {relative} to its reviewed pristine and patched digests"
            ));
        }
    }

    if manifest.reviewed_source_edits.len() != LORA_PHY_REVIEWED_EDIT_PATHS.len()
        || manifest
            .reviewed_source_edits
            .iter()
            .zip(LORA_PHY_REVIEWED_EDIT_PATHS)
            .any(|(edit, path)| {
                edit.path != path
                    || edit.upstream.is_empty()
                    || edit.vendored.is_empty()
                    || edit.upstream == edit.vendored
            })
    {
        return Err(
            "checked lora-phy manifest must describe the exact ordered sixteen reviewed edits across the SX126x core, variant, interface traits and public façade"
                .to_owned(),
        );
    }

    let mut expected_files = BTreeMap::new();
    for (role, files) in [
        ("unmodified upstream", &manifest.unmodified_upstream_files),
        ("project provenance", &manifest.project_files),
    ] {
        for (relative, digest) in files {
            validate_vendor_relative_path(relative)?;
            validate_sha256_digest(digest)?;
            if expected_files
                .insert(relative.clone(), digest.clone())
                .is_some()
            {
                return Err(format!(
                    "checked lora-phy manifest lists {relative:?} in more than one role ({role})"
                ));
            }
        }
    }
    for (relative, record) in &manifest.patched_upstream_files {
        validate_vendor_relative_path(relative)?;
        validate_sha256_digest(&record.upstream_sha256)?;
        validate_sha256_digest(&record.vendored_sha256)?;
        if expected_files
            .insert(relative.clone(), record.vendored_sha256.clone())
            .is_some()
        {
            return Err(format!(
                "checked lora-phy manifest lists patched file {relative:?} in more than one role"
            ));
        }
    }
    if expected_files
        .insert(LORA_PHY_VENDOR_MANIFEST.to_owned(), String::new())
        .is_some()
    {
        return Err(format!(
            "checked lora-phy manifest must not classify {LORA_PHY_VENDOR_MANIFEST} as payload"
        ));
    }

    let mut actual_files = BTreeSet::new();
    collect_vendor_files(vendor, vendor, &mut actual_files)?;
    let expected_paths = expected_files.keys().cloned().collect::<BTreeSet<_>>();
    if actual_files != expected_paths {
        let missing = expected_paths
            .difference(&actual_files)
            .cloned()
            .collect::<Vec<_>>();
        let unexpected = actual_files
            .difference(&expected_paths)
            .cloned()
            .collect::<Vec<_>>();
        return Err(format!(
            "lora-phy vendor tree differs from checked inventory; missing={missing:?}, unexpected={unexpected:?}"
        ));
    }

    for (relative, expected_digest) in expected_files {
        if relative == LORA_PHY_VENDOR_MANIFEST {
            continue;
        }
        let actual_digest = sha256_file(&vendor.join(&relative))?;
        if actual_digest != expected_digest {
            return Err(format!(
                "lora-phy vendor file {relative:?} digest {actual_digest} does not match checked {expected_digest}"
            ));
        }
    }

    let mut reconstructed = BTreeMap::new();
    for (relative, _, _) in LORA_PHY_PATCHED_UPSTREAM_FILES {
        let source = fs::read_to_string(vendor.join(relative))
            .map_err(|error| format!("could not read patched {relative}: {error}"))?;
        reconstructed.insert(relative, source);
    }
    for edit in &manifest.reviewed_source_edits {
        let source = reconstructed
            .get_mut(edit.path.as_str())
            .ok_or_else(|| format!("reviewed edit names unpatched file {:?}", edit.path))?;
        let vendored_occurrences = source.matches(&edit.vendored).count();
        if vendored_occurrences != 1 {
            return Err(format!(
                "reviewed lora-phy edit in {:?} has {vendored_occurrences} vendored occurrences, expected 1",
                edit.path
            ));
        }
        *source = source.replacen(&edit.vendored, &edit.upstream, 1);
    }
    for (relative, upstream_sha256, _) in LORA_PHY_PATCHED_UPSTREAM_FILES {
        let reconstructed_digest = sha256_bytes(
            reconstructed
                .get(relative)
                .expect("all reviewed patched files were loaded")
                .as_bytes(),
        );
        if reconstructed_digest != upstream_sha256 {
            return Err(format!(
                "reversing the reviewed edits produced {relative} digest {reconstructed_digest}, expected pristine {upstream_sha256}"
            ));
        }
    }

    Ok(())
}

fn collect_vendor_files(
    root: &Path,
    directory: &Path,
    files: &mut BTreeSet<String>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("could not enumerate {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "could not inspect an entry below {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|error| format!("could not inspect {}: {error}", path.display()))?;
        if file_type.is_symlink() {
            return Err(format!(
                "vendor tree contains forbidden symlink {}",
                path.display()
            ));
        }
        if file_type.is_dir() {
            collect_vendor_files(root, &path, files)?;
        } else if file_type.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|error| format!("could not relativize {}: {error}", path.display()))?
                .to_str()
                .ok_or_else(|| format!("vendor path {} is not UTF-8", path.display()))?
                .replace(std::path::MAIN_SEPARATOR, "/");
            files.insert(relative);
        } else {
            return Err(format!(
                "vendor tree contains unsupported entry {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn validate_vendor_relative_path(relative: &str) -> Result<(), String> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(format!(
            "checked vendor manifest contains unsafe path {relative:?}"
        ))
    } else {
        Ok(())
    }
}

fn validate_sha256_digest(digest: &str) -> Result<(), String> {
    if digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(format!("non-canonical SHA-256 digest {digest:?}"))
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read {} for hashing: {error}", path.display()))?;
    Ok(sha256_bytes(&bytes))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn capture<I, S>(program: &str, args: I) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;

    let mut combined = String::from_utf8_lossy(&output.stdout).into_owned();
    combined.push_str(&String::from_utf8_lossy(&output.stderr));
    if output.status.success() {
        Ok(combined)
    } else {
        Err(format!(
            "{program} exited with {}: {}",
            output.status,
            first_line(&combined)
        ))
    }
}

fn capture_stdout_at<I, S>(program: &str, args: I, current_dir: &Path) -> Result<String, String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = Command::new(program)
        .current_dir(current_dir)
        .args(args)
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("could not run {program}: {error}"))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(format!(
            "{program} exited with {}: {}",
            output.status,
            first_line(&stderr)
        ))
    }
}

fn run_inherited(program: &str, args: &[&str]) -> ExitCode {
    run_inherited_at(program, args, Path::new("."))
}

fn run_inherited_at(program: &str, args: &[&str], current_dir: &Path) -> ExitCode {
    match Command::new(program)
        .current_dir(current_dir)
        .args(args)
        .status()
    {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => {
            eprintln!("{program} exited with {status}");
            ExitCode::FAILURE
        }
        Err(error) => {
            eprintln!("could not run {program}: {error}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must live directly below the workspace root")
        .to_owned()
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_handles_empty_and_multiline_output() {
        assert_eq!(first_line(""), "");
        assert_eq!(first_line("one\ntwo"), "one");
    }

    #[test]
    fn workspace_root_contains_the_workspace_manifest() {
        assert!(workspace_root().join("Cargo.toml").is_file());
    }

    fn expected_pairing_publication_owner_sources() -> Vec<(String, String)> {
        CREDENTIAL_STORE_INTEGRATION_ALLOWED_SOURCES
            .iter()
            .enumerate()
            .map(|(source_index, path)| {
                let mut source = String::new();
                for expectation in &CREDENTIAL_STORE_INTEGRATION_ESCAPE_EXPECTATIONS {
                    for occurrence in 0..expectation.expected_occurrences_by_source[source_index] {
                        source.push_str(&format!(
                            "fn {}_{occurrence}() {{}}\n",
                            expectation.identifier
                        ));
                    }
                }
                ((*path).to_owned(), source)
            })
            .collect()
    }

    #[test]
    fn pairing_publication_escape_identifiers_are_owner_file_only() {
        let allowed = expected_pairing_publication_owner_sources();
        validate_pairing_publication_sources(&allowed).unwrap();

        for expectation in &CREDENTIAL_STORE_INTEGRATION_ESCAPE_EXPECTATIONS {
            let escaped_path = "crates/untrusted-pairing-composition/src/lib.rs";
            let mut escaped = allowed.clone();
            escaped.push((
                escaped_path.to_owned(),
                format!("fn bypass() {{ {}(); }}\n", expectation.identifier),
            ));
            let error = validate_pairing_publication_sources(&escaped)
                .expect_err("untrusted integration identifier was accepted");
            assert!(error.contains(expectation.identifier), "{error}");
            assert!(error.contains(escaped_path), "{error}");
        }
    }

    #[test]
    fn pairing_publication_escape_identifiers_require_exact_owner_sites() {
        let allowed = expected_pairing_publication_owner_sources();

        for expectation in &CREDENTIAL_STORE_INTEGRATION_ESCAPE_EXPECTATIONS {
            for path in CREDENTIAL_STORE_INTEGRATION_ALLOWED_SOURCES {
                let mut missing = allowed.clone();
                let source = &mut missing
                    .iter_mut()
                    .find(|(candidate, _)| candidate == path)
                    .unwrap()
                    .1;
                *source = source.replacen(expectation.identifier, "renamed_escape", 1);
                let error = validate_pairing_publication_sources(&missing)
                    .expect_err("a missing required owner occurrence was accepted");
                assert!(error.contains(expectation.identifier), "{error}");
                assert!(error.contains(path), "{error}");

                let mut extra = allowed.clone();
                extra
                    .iter_mut()
                    .find(|(candidate, _)| candidate == path)
                    .unwrap()
                    .1
                    .push_str(expectation.identifier);
                let error = validate_pairing_publication_sources(&extra)
                    .expect_err("an extra owner occurrence was accepted");
                assert!(error.contains(expectation.identifier), "{error}");
                assert!(error.contains(path), "{error}");
            }
        }

        let mut missing_owner = allowed;
        missing_owner.retain(|(path, _)| path != CREDENTIAL_STORE_INTEGRATION_ALLOWED_SOURCES[0]);
        let error = validate_pairing_publication_sources(&missing_owner)
            .expect_err("a missing owner source was accepted");
        assert!(
            error.contains(CREDENTIAL_STORE_INTEGRATION_ALLOWED_SOURCES[0]),
            "{error}"
        );
    }

    fn pairing_publication_coverage_metadata(
        root: &Path,
        package_roots: &[&str],
    ) -> serde_json::Value {
        let packages = package_roots
            .iter()
            .enumerate()
            .map(|(index, package_root)| {
                serde_json::json!({
                    "id": format!("fixture-{index}"),
                    "name": format!("fixture-{index}"),
                    "manifest_path": root.join(package_root).join("Cargo.toml"),
                    "targets": [{
                        "src_path": root.join(package_root).join("src/lib.rs")
                    }]
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "workspace_members": packages
                .iter()
                .map(|package| package["id"].clone())
                .collect::<Vec<_>>(),
            "packages": packages
        })
    }

    #[test]
    fn pairing_publication_scan_covers_every_workspace_member_source_root() {
        let root = Path::new("/workspace");
        let expected_roots = ["comparisons", "crates", "firmware", "tools", "xtask"];
        assert_eq!(PAIRING_PUBLICATION_SCAN_ROOTS, expected_roots);
        let metadata = pairing_publication_coverage_metadata(
            root,
            &[
                "comparisons/reference",
                "crates/library",
                "firmware/device",
                "tools/helper",
                "xtask",
            ],
        );
        validate_pairing_publication_workspace_member_coverage(&metadata.to_string(), root)
            .unwrap();

        let unscanned = pairing_publication_coverage_metadata(root, &["experiments/new-member"]);
        let error =
            validate_pairing_publication_workspace_member_coverage(&unscanned.to_string(), root)
                .expect_err("an unscanned workspace member root was accepted");
        assert!(error.contains("experiments"), "{error}");

        let mut outside_target = pairing_publication_coverage_metadata(root, &["crates/library"]);
        outside_target["packages"][0]["targets"][0]["src_path"] =
            serde_json::json!(root.join("generated/library.rs"));
        let error = validate_pairing_publication_workspace_member_coverage(
            &outside_target.to_string(),
            root,
        )
        .expect_err("an unscanned workspace member target was accepted");
        assert!(error.contains("generated"), "{error}");
    }

    #[test]
    fn current_workspace_respects_pairing_publication_source_boundary() {
        validate_pairing_publication_workspace(&workspace_root()).unwrap();
    }

    #[test]
    fn interface_neutral_rns_closure_rejects_rnode_and_lora_packages() {
        validate_interface_neutral_rns_closure(
            "node core",
            "reticulum-node-core v0.1.0\nreticulum-rns-rete v0.1.0\nrete-core v0.1.0",
        )
        .unwrap();

        for forbidden in INTERFACE_NEUTRAL_RNS_FORBIDDEN {
            assert!(
                validate_interface_neutral_rns_closure(
                    "node core",
                    &format!("reticulum-node-core v0.1.0\n{forbidden} v0.1.0"),
                )
                .is_err(),
                "accepted forbidden package {forbidden}"
            );
        }
    }

    #[test]
    fn resolved_rete_pin_is_tied_to_reported_metadata() {
        let source = "https://github.com/example/rete";
        let revision = "0123456789abcdef0123456789abcdef01234567";
        let expected = "git+https://github.com/example/rete.git?rev=\
                        0123456789abcdef0123456789abcdef01234567#\
                        0123456789abcdef0123456789abcdef01234567";
        let metadata = serde_json::json!({
            "packages": [
                { "name": "rete-core", "source": expected },
                { "name": "rete-stack", "source": expected },
                { "name": "rete-transport", "source": expected },
            ]
        });

        validate_resolved_rete_pin(&metadata.to_string(), source, revision).unwrap();

        let mut mismatched = metadata;
        mismatched["packages"][2]["source"] = serde_json::Value::String(
            "git+https://github.com/example/rete.git?rev=bad#bad".to_owned(),
        );
        assert!(validate_resolved_rete_pin(&mismatched.to_string(), source, revision).is_err());
    }

    #[test]
    fn resolved_esp_rtos_is_tied_to_reviewed_local_patch() {
        let root = workspace_root();
        let manifest = root.join("vendor/esp-rtos-0.3.0/Cargo.toml");
        let metadata = serde_json::json!({
            "packages": [{
                "name": "esp-rtos",
                "version": "0.3.0",
                "source": null,
                "manifest_path": manifest,
            }]
        });

        validate_resolved_esp_rtos_patch(&metadata.to_string(), &root).unwrap();

        let mut crates_io = metadata.clone();
        crates_io["packages"][0]["source"] = serde_json::Value::String(
            "registry+https://github.com/rust-lang/crates.io-index".into(),
        );
        assert!(validate_resolved_esp_rtos_patch(&crates_io.to_string(), &root).is_err());

        let mut wrong_path = metadata;
        wrong_path["packages"][0]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(validate_resolved_esp_rtos_patch(&wrong_path.to_string(), &root).is_err());
    }

    #[test]
    fn esp_rtos_vendor_tree_matches_checked_registry_inventory_and_two_edits() {
        let vendor = workspace_root().join("vendor/esp-rtos-0.3.0");
        validate_esp_rtos_vendor_tree(&vendor).unwrap();

        let manifest_text = fs::read_to_string(vendor.join(ESP_RTOS_VENDOR_MANIFEST)).unwrap();
        let manifest: VendorHashManifest = serde_json::from_str(&manifest_text).unwrap();

        let mut missing_file = manifest.clone();
        missing_file.unmodified_upstream_files.remove("README.md");
        assert!(validate_esp_rtos_vendor_tree_with_manifest(&vendor, &missing_file).is_err());

        let mut changed_edit = manifest.clone();
        changed_edit.reviewed_source_edits[1].vendored.push(' ');
        assert!(validate_esp_rtos_vendor_tree_with_manifest(&vendor, &changed_edit).is_err());

        let mut changed_digest = manifest;
        changed_digest
            .patched_upstream_files
            .get_mut("src/lib.rs")
            .unwrap()
            .vendored_sha256 = "0".repeat(64);
        assert!(validate_esp_rtos_vendor_tree_with_manifest(&vendor, &changed_digest).is_err());
    }

    #[test]
    fn resolved_lora_phy_is_tied_to_reviewed_local_patch() {
        let root = workspace_root();
        let manifest = root.join("vendor/lora-phy-3.0.1/Cargo.toml");
        let metadata = serde_json::json!({
            "packages": [{
                "name": "lora-phy",
                "version": "3.0.1",
                "source": null,
                "manifest_path": manifest,
            }]
        });

        validate_resolved_lora_phy_patch(&metadata.to_string(), &root).unwrap();

        let mut crates_io = metadata.clone();
        crates_io["packages"][0]["source"] = serde_json::Value::String(
            "registry+https://github.com/rust-lang/crates.io-index".into(),
        );
        assert!(validate_resolved_lora_phy_patch(&crates_io.to_string(), &root).is_err());

        let mut wrong_path = metadata;
        wrong_path["packages"][0]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(validate_resolved_lora_phy_patch(&wrong_path.to_string(), &root).is_err());
    }

    #[test]
    fn lora_phy_vendor_tree_matches_checked_registry_inventory_and_reviewed_edits() {
        let vendor = workspace_root().join("vendor/lora-phy-3.0.1");
        validate_lora_phy_vendor_tree(&vendor).unwrap();

        let manifest_text = fs::read_to_string(vendor.join(LORA_PHY_VENDOR_MANIFEST)).unwrap();
        let manifest: VendorHashManifest = serde_json::from_str(&manifest_text).unwrap();

        let mut missing_file = manifest.clone();
        missing_file.unmodified_upstream_files.remove("README.md");
        assert!(validate_lora_phy_vendor_tree_with_manifest(&vendor, &missing_file).is_err());

        let mut changed_edit = manifest.clone();
        changed_edit.reviewed_source_edits[6].vendored.push(' ');
        assert!(validate_lora_phy_vendor_tree_with_manifest(&vendor, &changed_edit).is_err());

        let mut changed_digest = manifest.clone();
        changed_digest
            .patched_upstream_files
            .get_mut("src/sx126x/mod.rs")
            .unwrap()
            .vendored_sha256 = "0".repeat(64);
        assert!(validate_lora_phy_vendor_tree_with_manifest(&vendor, &changed_digest).is_err());

        let mut changed_patches = manifest;
        changed_patches
            .project_files
            .insert("PATCHES.md".to_owned(), "0".repeat(64));
        assert!(validate_lora_phy_vendor_tree_with_manifest(&vendor, &changed_patches).is_err());
    }

    #[test]
    fn firmware_dependency_boundary_rejects_tx_ownership_and_full_rete_crates() {
        let root = workspace_root();
        let board_path = root.join("crates/board-heltec-tracker-v2");
        let radio_path = root.join("crates/radio-interface");
        let facade_path = root.join("crates/rns-rete-rx");
        let returned_fault_path = root.join("crates/lab-rx-returned-fault-hil");
        let metadata = serde_json::json!({
            "packages": [
                {
                    "id": "firmware-id",
                    "name": "reticulum-heltec-tracker-v2",
                    "dependencies": [
                        dependency_fixture("reticulum-board-heltec-tracker-v2", &board_path),
                        dependency_fixture("reticulum-radio-interface", &radio_path),
                        dependency_fixture("reticulum-rns-rete-rx", &facade_path),
                        optional_dependency_fixture(
                            "reticulum-lab-rx-returned-fault-hil",
                            &returned_fault_path,
                        ),
                    ]
                },
                package_fixture("board-id", "reticulum-board-heltec-tracker-v2", &board_path),
                package_fixture("radio-id", "reticulum-radio-interface", &radio_path),
                package_fixture("facade-id", "reticulum-rns-rete-rx", &facade_path),
            ],
            "resolve": {
                "nodes": [
                    {
                        "id": "firmware-id",
                        "deps": [
                            resolved_dependency_fixture("board-id"),
                            resolved_dependency_fixture("radio-id"),
                            resolved_dependency_fixture("facade-id"),
                        ]
                    },
                    { "id": "board-id", "deps": [] },
                    { "id": "radio-id", "deps": [] },
                    { "id": "facade-id", "deps": [] },
                ]
            }
        });
        validate_firmware_dependency_boundary(&metadata.to_string(), &root).unwrap();

        for forbidden in [
            "lora-phy",
            "reticulum-rns-rete",
            "reticulum-device-api-adapter",
            "reticulum-device-api-credential-store",
            "reticulum-device-api-credentials",
            "reticulum-device-api-framing",
            "reticulum-device-api-pairing",
            "reticulum-device-api-pairing-control",
            "reticulum-device-api-handoff",
            "reticulum-device-api-pairing-policy",
            "reticulum-device-api-session",
            "reticulum-node-core",
            "reticulum-radio-tx-dispatch",
            "reticulum-semantic-roundtrip-hil",
            "reticulum-storage-actor",
            "reticulum-storage-journal",
            "reticulum-storage-model",
            "reticulum-submission-projector",
            "reticulum-tx-dispatch",
            "reticulum-tx-handoff",
            "reticulum-tx-supervisor",
        ] {
            let mut prohibited = metadata.clone();
            prohibited["packages"][0]["dependencies"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({ "name": forbidden }));
            assert!(
                validate_firmware_dependency_boundary(&prohibited.to_string(), &root).is_err(),
                "direct firmware dependency {forbidden} was accepted"
            );
        }

        for (id, name, relative_path) in [
            (
                "device-api-adapter-id",
                "reticulum-device-api-adapter",
                "crates/device-api-adapter",
            ),
            (
                "device-api-credential-store-id",
                "reticulum-device-api-credential-store",
                "crates/device-api-credential-store",
            ),
            (
                "device-api-credentials-id",
                "reticulum-device-api-credentials",
                "crates/device-api-credentials",
            ),
            (
                "device-api-framing-id",
                "reticulum-device-api-framing",
                "crates/device-api-framing",
            ),
            (
                "device-api-pairing-control-id",
                "reticulum-device-api-pairing-control",
                "crates/device-api-pairing-control",
            ),
            (
                "device-api-pairing-id",
                "reticulum-device-api-pairing",
                "crates/device-api-pairing",
            ),
            (
                "device-api-handoff-id",
                "reticulum-device-api-handoff",
                "crates/device-api-handoff",
            ),
            (
                "device-api-pairing-policy-id",
                "reticulum-device-api-pairing-policy",
                "crates/device-api-pairing-policy",
            ),
            (
                "device-api-session-id",
                "reticulum-device-api-session",
                "crates/device-api-session",
            ),
            ("node-core-id", "reticulum-node-core", "crates/node-core"),
            (
                "radio-tx-dispatch-id",
                "reticulum-radio-tx-dispatch",
                "crates/radio-tx-dispatch",
            ),
            (
                "semantic-roundtrip-hil-id",
                "reticulum-semantic-roundtrip-hil",
                "crates/semantic-roundtrip-hil",
            ),
            (
                "storage-actor-id",
                "reticulum-storage-actor",
                "crates/storage-actor",
            ),
            (
                "storage-journal-id",
                "reticulum-storage-journal",
                "crates/storage-journal",
            ),
            (
                "storage-model-id",
                "reticulum-storage-model",
                "crates/storage-model",
            ),
            (
                "submission-projector-id",
                "reticulum-submission-projector",
                "crates/submission-projector",
            ),
            (
                "tx-dispatch-id",
                "reticulum-tx-dispatch",
                "crates/tx-dispatch",
            ),
            ("tx-handoff-id", "reticulum-tx-handoff", "crates/tx-handoff"),
            (
                "tx-supervisor-id",
                "reticulum-tx-supervisor",
                "crates/tx-supervisor",
            ),
        ] {
            let mut transitive = metadata.clone();
            transitive["packages"]
                .as_array_mut()
                .unwrap()
                .push(package_fixture(id, name, &root.join(relative_path)));
            transitive["resolve"]["nodes"][3]["deps"] =
                serde_json::json!([resolved_dependency_fixture(id)]);
            transitive["resolve"]["nodes"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({ "id": id, "deps": [] }));
            let error =
                validate_firmware_dependency_boundary(&transitive.to_string(), &root).unwrap_err();
            assert!(
                error.contains("prohibited pre-integration package"),
                "{error}"
            );
            assert!(error.contains(name), "{error}");
        }

        let mut incomplete_resolve = metadata.clone();
        incomplete_resolve["resolve"]["nodes"]
            .as_array_mut()
            .unwrap()
            .pop();
        assert!(
            validate_firmware_dependency_boundary(&incomplete_resolve.to_string(), &root).is_err(),
            "reachable package without a resolved node was accepted"
        );

        let mut optional = metadata;
        optional["packages"][0]["dependencies"][2]["optional"] = serde_json::Value::Bool(true);
        assert!(validate_firmware_dependency_boundary(&optional.to_string(), &root).is_err());
    }

    #[test]
    fn product_graph_boundary_rejects_feature_only_transitive_tx_ownership() {
        validate_product_graph_boundary(
            "all-features",
            "reticulum-heltec-tracker-v2 v0.1.0\n└── optional-rx-wrapper v0.1.0",
        )
        .unwrap();

        for forbidden in [
            "reticulum-board-heltec-tracker-v2-radio",
            "reticulum-board-heltec-tracker-v2-tx-hil",
            "reticulum-board-heltec-vision-master-e290-radio",
            "reticulum-device-api-adapter",
            "reticulum-device-api-credential-store",
            "reticulum-device-api-credentials",
            "reticulum-device-api-framing",
            "reticulum-device-api-pairing-control",
            "reticulum-device-api-pairing",
            "reticulum-device-api-handoff",
            "reticulum-device-api-pairing-policy",
            "reticulum-device-api-session",
            "reticulum-node-core",
            "reticulum-radio-tx-dispatch",
            "reticulum-radio-lora-phy",
            "reticulum-semantic-roundtrip-hil",
            "reticulum-storage-actor",
            "reticulum-storage-journal",
            "reticulum-storage-model",
            "reticulum-submission-projector",
            "reticulum-tx-dispatch",
            "reticulum-tx-handoff",
            "reticulum-tx-supervisor",
        ] {
            let tree = format!(
                "reticulum-heltec-tracker-v2 v0.1.0\n\
                 optional-future-feature-wrapper v0.1.0\n\
                 {forbidden} v0.1.0"
            );
            let error = validate_product_graph_boundary("all-features", &tree).unwrap_err();
            assert!(error.contains("all-features"));
            assert!(error.contains(forbidden));
        }
    }

    #[test]
    fn storage_hil_graph_boundary_rejects_radio_protocol_and_tx_packages() {
        validate_storage_hil_graph_boundary(
            "reticulum-heltec-tracker-v2-storage-hil v0.1.0\n└── esp-storage v0.9.0",
        )
        .unwrap();

        for forbidden in STORAGE_HIL_GRAPH_FORBIDDEN {
            let tree =
                format!("reticulum-heltec-tracker-v2-storage-hil v0.1.0\n└── {forbidden} v0.1.0");
            let error = validate_storage_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(forbidden), "{error}");
        }
    }

    #[test]
    fn hazardous_default_tx_hil_graph_requires_its_owner_and_excludes_protocol_stack() {
        let valid = "reticulum-heltec-tracker-v2-tx-hil v0.1.0\n\
                     ├── reticulum-board-heltec-tracker-v2-tx-hil v0.1.0\n\
                     │   └── reticulum-board-heltec-tracker-v2-radio v0.1.0\n\
                     ├── reticulum-radio-lora-phy v0.1.0\n\
                     ├── reticulum-radio-interface v0.1.0\n\
                     └── reticulum-semantic-roundtrip-hil v0.1.0 features=[]";
        validate_tx_hil_graph_boundary(valid).unwrap();

        for missing in TX_HIL_GRAPH_REQUIRED {
            let tree = valid.replace(missing, "missing-required-package");
            let error = validate_tx_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(missing), "{error}");
        }
        for forbidden in TX_HIL_GRAPH_FORBIDDEN {
            let tree = format!("{valid}\n└── {forbidden} v0.1.0");
            let error = validate_tx_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(forbidden), "{error}");
        }
    }

    #[test]
    fn semantic_tx_hil_graph_requires_exact_rete_surface_and_excludes_product_stack() {
        let valid = "reticulum-heltec-tracker-v2-tx-hil v0.1.0 features=[semantic-announce-hil,tracker-radio]\n\
                     ├── reticulum-board-heltec-tracker-v2-tx-hil v0.1.0 features=[]\n\
                     │   └── reticulum-board-heltec-tracker-v2-radio v0.1.0 features=[]\n\
                     ├── reticulum-radio-interface v0.1.0 features=[]\n\
                     ├── reticulum-radio-lora-phy v0.1.0 features=[]\n\
                     ├── reticulum-semantic-roundtrip-hil v0.1.0 features=[semantic-announce-hil]\n\
                     └── reticulum-rns-rete v0.1.0 features=[conformance]\n\
                         ├── rete-core v0.1.0 features=[alloc,default]\n\
                         ├── rete-stack v0.1.0 features=[alloc]\n\
                         └── rete-transport v0.1.0 features=[]";
        validate_semantic_tx_hil_graph_boundary(valid).unwrap();

        for missing in SEMANTIC_TX_HIL_GRAPH_REQUIRED {
            let tree = valid.replace(missing, "missing-required-package");
            let error = validate_semantic_tx_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(missing), "{error}");
        }
        for forbidden in SEMANTIC_TX_HIL_GRAPH_FORBIDDEN {
            let tree = format!("{valid}\n└── {forbidden} v0.1.0 features=[]");
            let error = validate_semantic_tx_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(forbidden), "{error}");
        }

        let no_root_feature = valid.replace(
            "features=[semantic-announce-hil,tracker-radio]",
            "features=[]",
        );
        assert!(validate_semantic_tx_hil_graph_boundary(&no_root_feature).is_err());

        let no_conformance = valid.replace(
            "reticulum-rns-rete v0.1.0 features=[conformance]",
            "reticulum-rns-rete v0.1.0 features=[]",
        );
        assert!(validate_semantic_tx_hil_graph_boundary(&no_conformance).is_err());
    }

    #[test]
    fn semantic_roundtrip_tx_hil_graph_requires_product_rete_surface_and_static_owner() {
        let valid = "reticulum-heltec-tracker-v2-tx-hil v0.1.0 features=[semantic-roundtrip-hil,tracker-radio]\n\
                     ├── reticulum-board-heltec-tracker-v2-tx-hil v0.1.0 features=[]\n\
                     │   └── reticulum-board-heltec-tracker-v2-radio v0.1.0 features=[]\n\
                     ├── reticulum-radio-interface v0.1.0 features=[]\n\
                     ├── reticulum-radio-lora-phy v0.1.0 features=[]\n\
                     ├── reticulum-semantic-roundtrip-hil v0.1.0 features=[semantic-roundtrip-hil]\n\
                     ├── reticulum-rns-rete v0.1.0 features=[]\n\
                     │   ├── rete-core v0.1.0 features=[alloc,default]\n\
                     │   ├── rete-stack v0.1.0 features=[alloc]\n\
                     │   └── rete-transport v0.1.0 features=[]\n\
                     └── static_cell v2.1.1 features=[]";
        validate_semantic_roundtrip_tx_hil_graph_boundary(valid).unwrap();

        for missing in SEMANTIC_TX_HIL_GRAPH_REQUIRED {
            let tree = valid.replace(missing, "missing-required-package");
            let error = validate_semantic_roundtrip_tx_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(missing), "{error}");
        }
        for forbidden in SEMANTIC_TX_HIL_GRAPH_FORBIDDEN {
            let tree = format!("{valid}\n└── {forbidden} v0.1.0 features=[]");
            let error = validate_semantic_roundtrip_tx_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(forbidden), "{error}");
        }

        let no_root_feature = valid.replace(
            "features=[semantic-roundtrip-hil,tracker-radio]",
            "features=[]",
        );
        assert!(validate_semantic_roundtrip_tx_hil_graph_boundary(&no_root_feature).is_err());

        let conformance = valid.replace(
            "reticulum-rns-rete v0.1.0 features=[]",
            "reticulum-rns-rete v0.1.0 features=[conformance]",
        );
        assert!(validate_semantic_roundtrip_tx_hil_graph_boundary(&conformance).is_err());

        let no_static = valid.replace("static_cell", "missing-static-owner");
        assert!(validate_semantic_roundtrip_tx_hil_graph_boundary(&no_static).is_err());
    }

    #[test]
    fn e290_semantic_hil_reuses_only_board_independent_policy() {
        let valid = "reticulum-heltec-vision-master-e290-semantic-hil v0.1.0 features=[]\n\
                     ├── reticulum-board-heltec-vision-master-e290-radio v0.1.0 features=[]\n\
                     │   ├── reticulum-board-heltec-vision-master-e290 v0.1.0 features=[]\n\
                     │   └── reticulum-radio-lora-phy v0.1.0 features=[]\n\
                     ├── reticulum-radio-interface v0.1.0 features=[]\n\
                     ├── reticulum-semantic-roundtrip-hil v0.1.0 features=[semantic-roundtrip-hil]\n\
                     └── reticulum-rns-rete v0.1.0 features=[]\n\
                         ├── rete-core v0.1.0 features=[alloc,default]\n\
                         ├── rete-stack v0.1.0 features=[alloc]\n\
                         └── rete-transport v0.1.0 features=[]";
        validate_e290_semantic_hil_graph_boundary(valid).unwrap();

        for missing in E290_SEMANTIC_HIL_GRAPH_REQUIRED {
            let tree = valid.replace(missing, "missing-required-package");
            let error = validate_e290_semantic_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(missing), "{error}");
        }
        for forbidden in E290_SEMANTIC_HIL_GRAPH_FORBIDDEN {
            let tree = format!("{valid}\n└── {forbidden} v0.1.0 features=[]");
            let error = validate_e290_semantic_hil_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(forbidden), "{error}");
        }

        let wrong_policy_feature = valid.replace(
            "features=[semantic-roundtrip-hil]",
            "features=[semantic-announce-hil,semantic-roundtrip-hil]",
        );
        assert!(validate_e290_semantic_hil_graph_boundary(&wrong_policy_feature).is_err());
    }

    fn e290_node_metadata_fixture(root: &Path) -> serde_json::Value {
        let mut esp_alloc = handoff_dependency_fixture("esp-alloc", "=0.10.0", None);
        esp_alloc["features"] =
            serde_json::json!(["esp32s3", "global-allocator", "internal-heap-stats"]);
        esp_alloc["target"] = serde_json::json!("cfg(target_arch = \"xtensa\")");
        esp_alloc["uses_default_features"] = serde_json::Value::Bool(true);
        let mut esp_println = handoff_dependency_fixture("esp-println", "=0.17.0", None);
        esp_println["features"] = serde_json::json!(["esp32s3", "log-04", "no-op"]);
        esp_println["target"] = serde_json::json!("cfg(target_arch = \"xtensa\")");
        let mut embedded_storage_target =
            handoff_dependency_fixture("embedded-storage", "=0.3.1", None);
        embedded_storage_target["target"] = serde_json::json!("cfg(target_arch = \"xtensa\")");
        let embedded_storage_dev =
            handoff_dependency_fixture("embedded-storage", "=0.3.1", Some("dev"));
        serde_json::json!({
            "packages": [{
                "name": "reticulum-heltec-vision-master-e290-node",
                "source": null,
                "manifest_path": root.join("firmware/heltec-vision-master-e290-node/Cargo.toml"),
                "features": {
                    "default": [],
                    "journal-schema2-dev-reprovision": [],
                    "rns-inbox-commit-fault-hil": [],
                    "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
                },
                "dependencies": [
                    handoff_path_dependency_fixture(
                        "reticulum-rns-inbox-store",
                        "*",
                        &root.join("crates/rns-inbox-store"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-device-api-credential-store",
                        "*",
                        &root.join("crates/device-api-credential-store"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-device-api-credentials",
                        "*",
                        &root.join("crates/device-api-credentials"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-device-api-pairing-policy",
                        "*",
                        &root.join("crates/device-api-pairing-policy"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-device-api-framing",
                        "*",
                        &root.join("crates/device-api-framing"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-device-api-handoff",
                        "*",
                        &root.join("crates/device-api-handoff"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-device-api-pairing",
                        "*",
                        &root.join("crates/device-api-pairing"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-device-api-pairing-control",
                        "*",
                        &root.join("crates/device-api-pairing-control"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-device-api-session",
                        "*",
                        &root.join("crates/device-api-session"),
                        None,
                    ),
                    embedded_storage_target,
                    embedded_storage_dev,
                    handoff_dependency_fixture("rand_core", "=0.6.4", None),
                    handoff_dependency_fixture("zeroize", "=1.9.0", None),
                    esp_alloc,
                    esp_println,
                ]
            }]
        })
    }

    #[test]
    fn permanent_e290_node_graph_is_lora_first_with_transport_neutral_durability() {
        let valid = "reticulum-heltec-vision-master-e290-node v0.1.0 features=[default]\n\
                     ├── embedded-storage v0.3.1 features=[]\n\
                     ├── esp-alloc v0.10.0 features=[compat,default,esp32s3,global-allocator,internal-heap-stats]\n\
                     ├── esp-println v0.17.0 features=[esp32s3,log-04,no-op]\n\
                     ├── esp-storage v0.9.0 features=[critical-section,esp32s3]\n\
                     ├── reticulum-announce-clock v0.1.0 features=[]\n\
                     ├── reticulum-board-heltec-vision-master-e290-radio v0.1.0 features=[]\n\
                     │   ├── reticulum-board-heltec-vision-master-e290 v0.1.0 features=[]\n\
                     │   └── reticulum-radio-lora-phy v0.1.0 features=[]\n\
                     ├── reticulum-device-api v0.1.0 features=[experimental-rns-data,experimental-rns-inbox]\n\
                     ├── reticulum-device-api-adapter v0.1.0 features=[experimental-rns-data,experimental-rns-inbox]\n\
                     ├── reticulum-device-api-credential-store v0.1.0 features=[]\n\
                     │   └── reticulum-device-api-credentials v0.1.0 features=[]\n\
                     ├── reticulum-device-api-framing v0.1.0 features=[]\n\
                     ├── reticulum-device-api-handoff v0.1.0 features=[]\n\
                     ├── reticulum-device-api-pairing v0.1.0 features=[]\n\
                     ├── reticulum-device-api-pairing-control v0.1.0 features=[]\n\
                     ├── reticulum-device-api-pairing-policy v0.1.0 features=[]\n\
                     ├── reticulum-device-api-session v0.1.0 features=[]\n\
                     ├── reticulum-device-identity-store v0.1.0 features=[]\n\
                     ├── reticulum-interface-router v0.1.0 features=[]\n\
                     ├── reticulum-node-core v0.1.0 features=[]\n\
                     │   └── reticulum-rns-rete v0.1.0 features=[]\n\
                     │       ├── rete-core v0.1.0 features=[alloc,default]\n\
                     │       ├── rete-stack v0.1.0 features=[alloc]\n\
                     │       └── rete-transport v0.1.0 features=[]\n\
                     ├── reticulum-nor-flash-region v0.1.0 features=[]\n\
                     ├── reticulum-radio-interface v0.1.0 features=[]\n\
                     ├── reticulum-radio-tx-dispatch v0.1.0 features=[]\n\
                     ├── reticulum-rns-inbox-store v0.1.0 features=[]\n\
                     ├── reticulum-storage-actor v0.1.0 features=[]\n\
                     ├── reticulum-storage-journal v0.1.0 features=[]\n\
                     ├── reticulum-storage-model v0.1.0 features=[]\n\
                     ├── reticulum-submission-projector v0.1.0 features=[]\n\
                     ├── reticulum-submission-runtime v0.1.0 features=[]\n\
                     ├── reticulum-tx-dispatch v0.1.0 features=[]\n\
                     ├── reticulum-tx-handoff v0.1.0 features=[]\n\
                     ├── reticulum-tx-supervisor v0.1.0 features=[]\n\
                     └── static_cell v2.1.1 features=[]";
        validate_e290_node_graph_boundary(valid).unwrap();
        let hil = valid.replacen(
            "features=[default]",
            "features=[rns-inbox-commit-fault-hil]",
            1,
        );
        validate_e290_inbox_commit_fault_hil_graph_boundary(valid, &hil).unwrap();
        let runtime_hil = valid
            .replacen(
                "features=[default]",
                "features=[runtime-measurement-hil]",
                1,
            )
            .replace(
                "features=[compat,default,esp32s3,global-allocator,internal-heap-stats]",
                "features=[alloc-hooks,compat,default,esp32s3,global-allocator,internal-heap-stats]",
            );
        validate_e290_runtime_measurement_hil_graph_boundary(valid, &runtime_hil).unwrap();

        for missing in E290_NODE_GRAPH_REQUIRED {
            let tree = valid.replace(missing, "missing-required-package");
            let error = validate_e290_node_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(missing), "{error}");
        }
        for forbidden in E290_NODE_GRAPH_FORBIDDEN {
            let tree = format!("{valid}\n└── {forbidden} v0.1.0 features=[]");
            let error = validate_e290_node_graph_boundary(&tree).unwrap_err();
            assert!(error.contains(forbidden), "{error}");
        }

        for hidden in [
            "default,journal-schema2-dev-reprovision",
            "default,rns-inbox-commit-fault-hil",
            "default,runtime-measurement-hil",
        ] {
            let hidden_feature = valid.replacen(
                "reticulum-heltec-vision-master-e290-node v0.1.0 features=[default]",
                &format!("reticulum-heltec-vision-master-e290-node v0.1.0 features=[{hidden}]"),
                1,
            );
            assert!(validate_e290_node_graph_boundary(&hidden_feature).is_err());
        }
        for hidden in [
            "default,rns-inbox-commit-fault-hil",
            "journal-schema2-dev-reprovision,rns-inbox-commit-fault-hil",
            "rns-inbox-commit-fault-hil,runtime-measurement-hil",
        ] {
            let hidden_feature = hil.replacen(
                "features=[rns-inbox-commit-fault-hil]",
                &format!("features=[{hidden}]"),
                1,
            );
            assert!(
                validate_e290_inbox_commit_fault_hil_graph_boundary(valid, &hidden_feature)
                    .is_err()
            );
        }
        let changed_hil_tail = format!("{hil}\n└── unreviewed-hil-edge v0.1.0 features=[]");
        assert!(
            validate_e290_inbox_commit_fault_hil_graph_boundary(valid, &changed_hil_tail).is_err()
        );

        for hidden in [
            "default,runtime-measurement-hil",
            "journal-schema2-dev-reprovision,runtime-measurement-hil",
            "rns-inbox-commit-fault-hil,runtime-measurement-hil",
        ] {
            let hidden_feature = runtime_hil.replacen(
                "features=[runtime-measurement-hil]",
                &format!("features=[{hidden}]"),
                1,
            );
            assert!(
                validate_e290_runtime_measurement_hil_graph_boundary(valid, &hidden_feature)
                    .is_err()
            );
        }
        for changed in [
            runtime_hil.replace("alloc-hooks,", ""),
            runtime_hil.replace(
                "alloc-hooks,compat",
                "alloc-hooks,compat,unreviewed-feature",
            ),
            format!("{runtime_hil}\n└── unreviewed-runtime-measurement-edge v0.1.0 features=[]"),
        ] {
            assert!(validate_e290_runtime_measurement_hil_graph_boundary(valid, &changed).is_err());
        }

        for package in ["reticulum-device-api", "reticulum-device-api-adapter"] {
            let expected =
                format!("{package} v0.1.0 features=[experimental-rns-data,experimental-rns-inbox]");
            let drifted = format!("{package} v0.1.0 features=[]");
            let feature_drift = valid.replacen(&expected, &drifted, 1);
            assert!(
                validate_e290_node_graph_boundary(&feature_drift).is_err(),
                "permanent node accepted missing experimental feature on {package}"
            );
        }
        for package in [
            "reticulum-rns-inbox-store",
            "reticulum-device-api-credential-store",
            "reticulum-device-api-credentials",
            "reticulum-device-api-framing",
            "reticulum-device-api-handoff",
            "reticulum-device-api-pairing",
            "reticulum-device-api-pairing-control",
            "reticulum-device-api-pairing-policy",
            "reticulum-device-api-session",
        ] {
            let expected = format!("{package} v0.1.0 features=[]");
            let drifted = format!("{package} v0.1.0 features=[default]");
            let feature_drift = valid.replacen(&expected, &drifted, 1);
            assert!(
                validate_e290_node_graph_boundary(&feature_drift).is_err(),
                "permanent node accepted feature drift on feature-free package {package}"
            );
        }

        for forbidden_backend in ["auto", "jtag-serial", "uart"] {
            let feature_drift = valid.replacen(
                "esp-println v0.17.0 features=[esp32s3,log-04,no-op]",
                &format!("esp-println v0.17.0 features=[esp32s3,log-04,{forbidden_backend}]"),
                1,
            );
            assert!(
                validate_e290_node_graph_boundary(&feature_drift).is_err(),
                "permanent node accepted the {forbidden_backend} esp-println backend"
            );
        }
    }

    #[test]
    fn permanent_e290_node_development_features_remain_exact_and_opt_in() {
        let root = workspace_root();
        let baseline = e290_node_metadata_fixture(&root);
        validate_e290_node_feature_boundary(&baseline.to_string(), &root).unwrap();

        for drifted_features in [
            serde_json::json!({
                "default": ["journal-schema2-dev-reprovision"],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": [],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
            }),
            serde_json::json!({
                "default": ["rns-inbox-commit-fault-hil"],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": [],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
            }),
            serde_json::json!({
                "default": ["runtime-measurement-hil"],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": [],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
            }),
            serde_json::json!({
                "default": [],
                "journal-schema2-dev-reprovision": ["dep:unreviewed"],
                "rns-inbox-commit-fault-hil": [],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
            }),
            serde_json::json!({
                "default": [],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": ["dep:unreviewed"],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
            }),
            serde_json::json!({
                "default": [],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": [],
                "runtime-measurement-hil": []
            }),
            serde_json::json!({
                "default": [],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": [],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks", "dep:unreviewed"]
            }),
            serde_json::json!({
                "default": [],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": [],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks"],
                "future-transport": []
            }),
            serde_json::json!({
                "default": [],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": []
            }),
            serde_json::json!({
                "default": [],
                "rns-inbox-commit-fault-hil": [],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
            }),
            serde_json::json!({
                "default": [],
                "journal-schema2-dev-reprovision": [],
                "runtime-measurement-hil": ["esp-alloc/alloc-hooks"]
            }),
            serde_json::json!({
                "default": [],
                "journal-schema2-dev-reprovision": [],
                "rns-inbox-commit-fault-hil": []
            }),
        ] {
            let mut drifted = baseline.clone();
            drifted["packages"][0]["features"] = drifted_features;
            assert!(validate_e290_node_feature_boundary(&drifted.to_string(), &root).is_err());
        }
    }

    #[test]
    fn e290_commit_fault_hil_source_topology_is_narrow_and_feature_gated() {
        let library = r#"#[cfg(all(
    feature = "rns-inbox-commit-fault-hil",
    any(test, target_arch = "xtensa")
))]
pub mod inbox_admission_fault_hil;
"#;
        let storage = "pub(crate) fn offer_inbound(\n\
                       ) {\n\
                           let region = PartitionNorFlash::new();\n\
                           #[cfg(feature = \"rns-inbox-commit-fault-hil\")]\n\
                           let region = SuppressThirdWrite::new(region);\n\
                           self.inbox_service_enabled = false;\n\
                           self.record_inbound_drop();\n\
                           #[cfg(feature = \"rns-inbox-commit-fault-hil\")]\n\
                           observe_product_quarantine(&error, true, 1);\n\
                       }\n\
                       \n\
                           /// Count one input discarded\n";
        let build = "CARGO_FEATURE_JOURNAL_SCHEMA2_DEV_REPROVISION\n\
                     CARGO_FEATURE_RNS_INBOX_COMMIT_FAULT_HIL\n\
                     CARGO_FEATURE_RUNTIME_MEASUREMENT_HIL\n\
                     journal-schema2-dev-reprovision, rns-inbox-commit-fault-hil, and runtime-measurement-hil are mutually exclusive";
        let fixture = "#[used]\n\
                       pub static RETICULUM_INBOX_ADMISSION_FAULT_HIL_EVIDENCE: Evidence = Evidence;\n\
                       pub struct SuppressThirdWrite<F>(F);";
        validate_e290_inbox_commit_fault_hil_sources(library, storage, build, fixture).unwrap();

        let ungated_library = library.replace(
            "#[cfg(all(\n    feature = \"rns-inbox-commit-fault-hil\",\n    any(test, target_arch = \"xtensa\")\n))]\n",
            "",
        );
        assert!(
            validate_e290_inbox_commit_fault_hil_sources(
                &ungated_library,
                storage,
                build,
                fixture,
            )
            .is_err()
        );
        for ungated in [
            storage.replacen("#[cfg(feature = \"rns-inbox-commit-fault-hil\")]\n", "", 1),
            storage.replacen("#[cfg(feature = \"rns-inbox-commit-fault-hil\")]\n", "", 2),
            storage.replace(
                "self.record_inbound_drop();\n",
                "observe_product_quarantine(&error, true, 1);\nself.record_inbound_drop();\n",
            ),
        ] {
            assert!(
                validate_e290_inbox_commit_fault_hil_sources(library, &ungated, build, fixture,)
                    .is_err()
            );
        }

        assert!(
            validate_e290_inbox_commit_fault_hil_sources(
                library,
                storage,
                &build.replace("CARGO_FEATURE_RNS_INBOX_COMMIT_FAULT_HIL", "missing"),
                fixture,
            )
            .is_err()
        );
        assert!(
            validate_e290_inbox_commit_fault_hil_sources(
                library,
                storage,
                build,
                &format!("{fixture}\n#[unsafe(no_mangle)]"),
            )
            .is_err()
        );
    }

    fn e290_runtime_measurement_source_fixture() -> (Vec<(String, String)>, String) {
        let product = "firmware/heltec-vision-master-e290-node/src";
        let library = r#"#[cfg(feature = "runtime-measurement-hil")]
pub mod runtime_measurement;
"#;
        let main = r#"#[cfg(feature = "runtime-measurement-hil")]
mod runtime_measurement_stack_hil;

#[cfg(feature = "runtime-measurement-hil")]
use product::runtime_measurement::{
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE, RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE,
};

fn main() {
    #[cfg(feature = "runtime-measurement-hil")]
    let _monitor = runtime_measurement_stack_hil::Monitor::initialize();
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.sample();
    #[cfg(feature = "runtime-measurement-hil")]
    RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.sample();
}

#[cfg(feature = "runtime-measurement-hil")]
#[unsafe(no_mangle)]
fn _esp_alloc_alloc(
    heap: &::esp_alloc::EspHeap,
    _capabilities: ::esp_alloc::export::enumset::EnumSet<::esp_alloc::MemoryCapability>,
    pointer: usize,
    _size: usize,
) {
}

#[cfg(feature = "runtime-measurement-hil")]
#[unsafe(no_mangle)]
fn _esp_alloc_dealloc(_heap: &::esp_alloc::EspHeap, _pointer: usize, _size: usize) {}
"#;
        let runtime = r#"pub struct Evidence;

impl Evidence {
    pub fn sample(&self) {}
}

#[used]
pub static RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE: Evidence = Evidence;

#[used]
pub static RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE: Evidence = Evidence;
"#;
        let stack = r#"#[used]
#[unsafe(no_mangle)]
static mut RETICULUM_RUNTIME_MEASUREMENT_STACK_MARKER: u32 = 0;

core::arch::global_asm!("__zero_bss");

fn sample(layout: Layout) {
    scan_stack_watermark(layout, |address| {
        let innermost_sp = current_stack_pointer();
        if address >= innermost_sp {
            !stack_watermark_word(address)
        } else {
            read_word(address)
        }
    });
}
"#;
        let radio = r#"fn run() {
    #[cfg(feature = "runtime-measurement-hil")]
    product::runtime_measurement::RETICULUM_RUNTIME_MEASUREMENT_EVIDENCE.sample();
    #[cfg(feature = "runtime-measurement-hil")]
    product::runtime_measurement::RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.sample();
}
"#;
        let build = "CARGO_FEATURE_RUNTIME_MEASUREMENT_HIL\n\
                     journal-schema2-dev-reprovision, rns-inbox-commit-fault-hil, and runtime-measurement-hil are mutually exclusive";
        (
            vec![
                (format!("{product}/lib.rs"), library.to_owned()),
                (format!("{product}/main.rs"), main.to_owned()),
                (
                    format!("{product}/runtime_measurement.rs"),
                    runtime.to_owned(),
                ),
                (
                    format!("{product}/runtime_measurement_stack_hil.rs"),
                    stack.to_owned(),
                ),
                (format!("{product}/radio_task.rs"), radio.to_owned()),
            ],
            build.to_owned(),
        )
    }

    #[test]
    fn e290_runtime_measurement_sources_are_exactly_feature_contained() {
        let (sources, build) = e290_runtime_measurement_source_fixture();
        validate_e290_runtime_measurement_hil_sources(&sources, &build).unwrap();

        let mutate = |path: &str, rewrite: &dyn Fn(&str) -> String| {
            sources
                .iter()
                .map(|(candidate, source)| {
                    if candidate.ends_with(path) {
                        (candidate.clone(), rewrite(source))
                    } else {
                        (candidate.clone(), source.clone())
                    }
                })
                .collect::<Vec<_>>()
        };

        let ungated_library = mutate("/lib.rs", &|source| {
            source.replace("#[cfg(feature = \"runtime-measurement-hil\")]\n", "")
        });
        assert!(validate_e290_runtime_measurement_hil_sources(&ungated_library, &build).is_err());

        let ungated_stack_module = mutate("/main.rs", &|source| {
            source.replacen("#[cfg(feature = \"runtime-measurement-hil\")]\n", "", 1)
        });
        assert!(
            validate_e290_runtime_measurement_hil_sources(&ungated_stack_module, &build).is_err()
        );

        let ungated_stack_call = mutate("/main.rs", &|source| {
            source.replace(
                "    #[cfg(feature = \"runtime-measurement-hil\")]\n    let _monitor = runtime_measurement_stack_hil::Monitor::initialize();",
                "    let _monitor = runtime_measurement_stack_hil::Monitor::initialize();",
            )
        });
        assert!(
            validate_e290_runtime_measurement_hil_sources(&ungated_stack_call, &build).is_err()
        );

        let ungated_evidence_use = mutate("/radio_task.rs", &|source| {
            source.replace("    #[cfg(feature = \"runtime-measurement-hil\")]\n", "")
        });
        assert!(
            validate_e290_runtime_measurement_hil_sources(&ungated_evidence_use, &build).is_err()
        );

        let ungated_proof_trace_use = mutate("/radio_task.rs", &|source| {
            source.replace(
                "    #[cfg(feature = \"runtime-measurement-hil\")]\n    product::runtime_measurement::RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.sample();",
                "    product::runtime_measurement::RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE.sample();",
            )
        });
        assert!(
            validate_e290_runtime_measurement_hil_sources(&ungated_proof_trace_use, &build)
                .is_err()
        );

        let ungated_hook = mutate("/main.rs", &|source| {
            source.replace(
                "#[cfg(feature = \"runtime-measurement-hil\")]\n#[unsafe(no_mangle)]\nfn _esp_alloc_alloc",
                "#[unsafe(no_mangle)]\nfn _esp_alloc_alloc",
            )
        });
        assert!(validate_e290_runtime_measurement_hil_sources(&ungated_hook, &build).is_err());

        let unretained_evidence = mutate("/runtime_measurement.rs", &|source| {
            source.replace("#[used]\n", "")
        });
        assert!(
            validate_e290_runtime_measurement_hil_sources(&unretained_evidence, &build).is_err()
        );
        let unretained_proof_trace = mutate("/runtime_measurement.rs", &|source| {
            source.replace(
                "#[used]\npub static RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE",
                "pub static RETICULUM_RUNTIME_PROOF_TRACE_EVIDENCE",
            )
        });
        assert!(
            validate_e290_runtime_measurement_hil_sources(&unretained_proof_trace, &build).is_err()
        );
        let unmangled_evidence = mutate("/runtime_measurement.rs", &|source| {
            source.replace("#[used]\n", "#[used]\n#[unsafe(no_mangle)]\n")
        });
        assert!(
            validate_e290_runtime_measurement_hil_sources(&unmangled_evidence, &build).is_err()
        );

        let unchecked_live_frame = mutate("/runtime_measurement_stack_hil.rs", &|source| {
            source.replace("if address >= innermost_sp {", "if false {")
        });
        assert!(
            validate_e290_runtime_measurement_hil_sources(&unchecked_live_frame, &build).is_err()
        );

        let missing_hook = mutate("/main.rs", &|source| {
            source.replace(
                "#[cfg(feature = \"runtime-measurement-hil\")]\n#[unsafe(no_mangle)]\nfn _esp_alloc_dealloc(_heap: &::esp_alloc::EspHeap, _pointer: usize, _size: usize) {}\n",
                "",
            )
        });
        assert!(validate_e290_runtime_measurement_hil_sources(&missing_hook, &build).is_err());

        let wrong_hook_abi = mutate("/main.rs", &|source| {
            source.replacen("pointer: usize", "pointer: u32", 1)
        });
        assert!(validate_e290_runtime_measurement_hil_sources(&wrong_hook_abi, &build).is_err());

        let mangled_hook = mutate("/main.rs", &|source| {
            source.replacen(
                "#[unsafe(no_mangle)]\nfn _esp_alloc_alloc",
                "fn _esp_alloc_alloc",
                1,
            )
        });
        assert!(validate_e290_runtime_measurement_hil_sources(&mangled_hook, &build).is_err());

        assert!(
            validate_e290_runtime_measurement_hil_sources(
                &sources,
                &build.replace("CARGO_FEATURE_RUNTIME_MEASUREMENT_HIL", "missing"),
            )
            .is_err()
        );
    }

    #[test]
    fn permanent_e290_node_requires_exact_direct_authentication_dependencies() {
        let root = workspace_root();
        let baseline = e290_node_metadata_fixture(&root);
        validate_e290_node_feature_boundary(&baseline.to_string(), &root).unwrap();

        for (dependency_name, wrong_path, rename) in [
            (
                "reticulum-rns-inbox-store",
                "crates/not-the-rns-inbox-store",
                "rns-inbox-store",
            ),
            (
                "reticulum-device-api-credential-store",
                "crates/not-the-credential-store",
                "credential-store",
            ),
            (
                "reticulum-device-api-pairing-policy",
                "crates/not-the-pairing-policy",
                "pairing-policy",
            ),
            (
                "reticulum-device-api-credentials",
                "crates/not-the-credentials",
                "credentials",
            ),
            (
                "reticulum-device-api-framing",
                "crates/not-device-api-framing",
                "device-api-framing",
            ),
            (
                "reticulum-device-api-handoff",
                "crates/not-device-api-handoff",
                "device-api-handoff",
            ),
            (
                "reticulum-device-api-pairing",
                "crates/not-device-api-pairing",
                "device-api-pairing",
            ),
            (
                "reticulum-device-api-pairing-control",
                "crates/not-device-api-pairing-control",
                "device-api-pairing-control",
            ),
            (
                "reticulum-device-api-session",
                "crates/not-device-api-session",
                "device-api-session",
            ),
        ] {
            let mut missing = baseline.clone();
            fixture_package_mut(&mut missing, "reticulum-heltec-vision-master-e290-node")
                ["dependencies"]
                .as_array_mut()
                .unwrap()
                .retain(|dependency| dependency["name"].as_str() != Some(dependency_name));
            assert!(
                validate_e290_node_feature_boundary(&missing.to_string(), &root).is_err(),
                "permanent node accepted missing {dependency_name}"
            );

            let mut duplicated = baseline.clone();
            let duplicate = duplicated["packages"][0]["dependencies"]
                .as_array()
                .unwrap()
                .iter()
                .find(|dependency| dependency["name"].as_str() == Some(dependency_name))
                .unwrap()
                .clone();
            duplicated["packages"][0]["dependencies"]
                .as_array_mut()
                .unwrap()
                .push(duplicate);
            assert!(
                validate_e290_node_feature_boundary(&duplicated.to_string(), &root).is_err(),
                "permanent node accepted duplicate {dependency_name}"
            );

            for (label, field, value) in [
                ("version requirement", "req", serde_json::json!("^0.1")),
                (
                    "registry source",
                    "source",
                    serde_json::json!("registry+https://github.com/rust-lang/crates.io-index"),
                ),
                (
                    "local path",
                    "path",
                    serde_json::json!(root.join(wrong_path)),
                ),
                ("dependency kind", "kind", serde_json::json!("dev")),
                ("optional edge", "optional", serde_json::json!(true)),
                ("renamed edge", "rename", serde_json::json!(rename)),
                (
                    "target-specific edge",
                    "target",
                    serde_json::json!("cfg(target_os = \"none\")"),
                ),
                (
                    "default features",
                    "uses_default_features",
                    serde_json::json!(true),
                ),
                (
                    "explicit feature",
                    "features",
                    serde_json::json!(["default"]),
                ),
            ] {
                let mut drifted = baseline.clone();
                let package =
                    fixture_package_mut(&mut drifted, "reticulum-heltec-vision-master-e290-node");
                fixture_dependency_mut(package, dependency_name, None)[field] = value;
                assert!(
                    validate_e290_node_feature_boundary(&drifted.to_string(), &root).is_err(),
                    "permanent node accepted {dependency_name} {label} drift"
                );
            }
        }

        for (dependency_name, requirement) in [
            ("embedded-storage", "=0.3.1"),
            ("rand_core", "=0.6.4"),
            ("zeroize", "=1.9.0"),
        ] {
            let mut missing = baseline.clone();
            fixture_package_mut(&mut missing, "reticulum-heltec-vision-master-e290-node")
                ["dependencies"]
                .as_array_mut()
                .unwrap()
                .retain(|dependency| dependency["name"].as_str() != Some(dependency_name));
            assert!(
                validate_e290_node_feature_boundary(&missing.to_string(), &root).is_err(),
                "permanent node accepted missing {dependency_name}"
            );

            let mut drifted = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut drifted, "reticulum-heltec-vision-master-e290-node"),
                dependency_name,
                None,
            )["req"] = serde_json::json!(format!("^{requirement}"));
            assert!(
                validate_e290_node_feature_boundary(&drifted.to_string(), &root).is_err(),
                "permanent node accepted {dependency_name} version drift"
            );

            let mut defaults = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut defaults, "reticulum-heltec-vision-master-e290-node"),
                dependency_name,
                None,
            )["uses_default_features"] = serde_json::json!(true);
            assert!(
                validate_e290_node_feature_boundary(&defaults.to_string(), &root).is_err(),
                "permanent node accepted {dependency_name} default features"
            );
        }

        for (field, value) in [
            ("kind", serde_json::json!("dev")),
            ("target", serde_json::Value::Null),
            ("optional", serde_json::json!(true)),
            ("features", serde_json::json!(["default"])),
        ] {
            let mut drifted = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut drifted, "reticulum-heltec-vision-master-e290-node"),
                "embedded-storage",
                None,
            )[field] = value;
            assert!(
                validate_e290_node_feature_boundary(&drifted.to_string(), &root).is_err(),
                "permanent node accepted embedded-storage {field} drift"
            );
        }
        for (field, value) in [
            ("kind", serde_json::Value::Null),
            ("target", serde_json::json!("cfg(target_arch = \"xtensa\")")),
            ("req", serde_json::json!("^0.3.1")),
            ("optional", serde_json::json!(true)),
            ("uses_default_features", serde_json::json!(true)),
            ("features", serde_json::json!(["default"])),
        ] {
            let mut drifted = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut drifted, "reticulum-heltec-vision-master-e290-node"),
                "embedded-storage",
                Some("dev"),
            )[field] = value;
            assert!(
                validate_e290_node_feature_boundary(&drifted.to_string(), &root).is_err(),
                "permanent node accepted dev embedded-storage {field} drift"
            );
        }

        let dependency_name = "esp-println";
        let mut missing = baseline.clone();
        fixture_package_mut(&mut missing, "reticulum-heltec-vision-master-e290-node")
            ["dependencies"]
            .as_array_mut()
            .unwrap()
            .retain(|dependency| dependency["name"].as_str() != Some(dependency_name));
        assert!(
            validate_e290_node_feature_boundary(&missing.to_string(), &root).is_err(),
            "permanent node accepted missing esp-println"
        );

        let mut duplicated = baseline.clone();
        let duplicate = duplicated["packages"][0]["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|dependency| dependency["name"].as_str() == Some(dependency_name))
            .unwrap()
            .clone();
        duplicated["packages"][0]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(
            validate_e290_node_feature_boundary(&duplicated.to_string(), &root).is_err(),
            "permanent node accepted duplicate esp-println"
        );

        for forbidden_backend in ["auto", "jtag-serial", "uart"] {
            for features in [
                serde_json::json!(["esp32s3", "log-04", forbidden_backend]),
                serde_json::json!(["esp32s3", "log-04", "no-op", forbidden_backend]),
            ] {
                let mut drifted = baseline.clone();
                fixture_dependency_mut(
                    fixture_package_mut(&mut drifted, "reticulum-heltec-vision-master-e290-node"),
                    dependency_name,
                    None,
                )["features"] = features;
                assert!(
                    validate_e290_node_feature_boundary(&drifted.to_string(), &root).is_err(),
                    "permanent node accepted the {forbidden_backend} esp-println backend"
                );
            }
        }

        for (label, field, value) in [
            ("version requirement", "req", serde_json::json!("^0.17")),
            (
                "registry source",
                "source",
                serde_json::json!("registry+https://example.invalid/index"),
            ),
            (
                "local path",
                "path",
                serde_json::json!(root.join("vendor/lookalike-esp-println")),
            ),
            ("dependency kind", "kind", serde_json::json!("dev")),
            ("optional edge", "optional", serde_json::json!(true)),
            ("renamed edge", "rename", serde_json::json!("silent-logger")),
            ("missing target", "target", serde_json::Value::Null),
            (
                "wrong target",
                "target",
                serde_json::json!("cfg(target_os = \"none\")"),
            ),
            (
                "default features",
                "uses_default_features",
                serde_json::json!(true),
            ),
            (
                "feature set",
                "features",
                serde_json::json!(["esp32s3", "no-op"]),
            ),
        ] {
            let mut drifted = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut drifted, "reticulum-heltec-vision-master-e290-node"),
                dependency_name,
                None,
            )[field] = value;
            assert!(
                validate_e290_node_feature_boundary(&drifted.to_string(), &root).is_err(),
                "permanent node accepted esp-println {label} drift"
            );
        }

        let mut missing_esp_alloc = baseline.clone();
        fixture_package_mut(
            &mut missing_esp_alloc,
            "reticulum-heltec-vision-master-e290-node",
        )["dependencies"]
            .as_array_mut()
            .unwrap()
            .retain(|dependency| dependency["name"].as_str() != Some("esp-alloc"));
        assert!(
            validate_e290_node_feature_boundary(&missing_esp_alloc.to_string(), &root).is_err()
        );
        for (field, value) in [
            ("req", serde_json::json!("^0.10")),
            (
                "source",
                serde_json::json!("registry+https://example.invalid/index"),
            ),
            (
                "path",
                serde_json::json!(root.join("vendor/lookalike-esp-alloc")),
            ),
            ("kind", serde_json::json!("dev")),
            ("optional", serde_json::json!(true)),
            ("rename", serde_json::json!("allocator")),
            ("target", serde_json::Value::Null),
            ("uses_default_features", serde_json::json!(false)),
            (
                "features",
                serde_json::json!([
                    "alloc-hooks",
                    "esp32s3",
                    "global-allocator",
                    "internal-heap-stats"
                ]),
            ),
        ] {
            let mut drifted = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut drifted, "reticulum-heltec-vision-master-e290-node"),
                "esp-alloc",
                None,
            )[field] = value;
            assert!(
                validate_e290_node_feature_boundary(&drifted.to_string(), &root).is_err(),
                "permanent node accepted esp-alloc {field} drift"
            );
        }

        let mut wrong_manifest = baseline.clone();
        fixture_package_mut(
            &mut wrong_manifest,
            "reticulum-heltec-vision-master-e290-node",
        )["manifest_path"] = serde_json::json!(
            root.join("firmware/not-the-heltec-vision-master-e290-node/Cargo.toml")
        );
        assert!(validate_e290_node_feature_boundary(&wrong_manifest.to_string(), &root).is_err());
    }

    #[test]
    fn credential_and_inbox_stores_remain_forbidden_from_legacy_product_and_hil_graphs() {
        for (label, forbidden) in [
            ("Tracker product", &PRODUCT_GRAPH_FORBIDDEN[..]),
            ("storage HIL", &STORAGE_HIL_GRAPH_FORBIDDEN[..]),
            ("Tracker TX HIL", &TX_HIL_GRAPH_FORBIDDEN[..]),
            (
                "Tracker semantic TX HIL",
                &SEMANTIC_TX_HIL_GRAPH_FORBIDDEN[..],
            ),
            ("E290 semantic HIL", &E290_SEMANTIC_HIL_GRAPH_FORBIDDEN[..]),
        ] {
            for package in [
                "reticulum-device-api-credential-store",
                "reticulum-device-api-credentials",
                "reticulum-rns-inbox-store",
            ] {
                assert!(
                    forbidden.contains(&package),
                    "{label} no longer forbids {package}"
                );
            }
        }
        assert!(E290_NODE_GRAPH_REQUIRED.contains(&"reticulum-rns-inbox-store"));
        assert!(!E290_NODE_GRAPH_FORBIDDEN.contains(&"reticulum-rns-inbox-store"));
    }

    #[test]
    fn pairing_policy_is_required_only_by_permanent_e290_node() {
        for (label, forbidden) in [
            ("Tracker product", &PRODUCT_GRAPH_FORBIDDEN[..]),
            ("storage HIL", &STORAGE_HIL_GRAPH_FORBIDDEN[..]),
            ("Tracker TX HIL", &TX_HIL_GRAPH_FORBIDDEN[..]),
            (
                "Tracker semantic TX HIL",
                &SEMANTIC_TX_HIL_GRAPH_FORBIDDEN[..],
            ),
            ("E290 semantic HIL", &E290_SEMANTIC_HIL_GRAPH_FORBIDDEN[..]),
        ] {
            assert!(
                forbidden.contains(&"reticulum-device-api-pairing-policy"),
                "{label} no longer forbids the pairing policy"
            );
        }
        assert!(E290_NODE_GRAPH_REQUIRED.contains(&"reticulum-device-api-pairing-policy"));
        assert!(!E290_NODE_GRAPH_FORBIDDEN.contains(&"reticulum-device-api-pairing-policy"));
    }

    #[test]
    fn pairing_control_is_required_only_by_permanent_e290_usb_composition() {
        for (label, forbidden) in [
            ("Tracker product", &PRODUCT_GRAPH_FORBIDDEN[..]),
            ("storage HIL", &STORAGE_HIL_GRAPH_FORBIDDEN[..]),
            ("Tracker TX HIL", &TX_HIL_GRAPH_FORBIDDEN[..]),
            (
                "Tracker semantic TX HIL",
                &SEMANTIC_TX_HIL_GRAPH_FORBIDDEN[..],
            ),
            ("E290 semantic HIL", &E290_SEMANTIC_HIL_GRAPH_FORBIDDEN[..]),
        ] {
            for package in [
                "reticulum-device-api-framing",
                "reticulum-device-api-pairing-control",
            ] {
                assert!(
                    forbidden.contains(&package),
                    "{label} no longer forbids the E290 USB pre-authentication package {package}"
                );
            }
        }
        for package in [
            "reticulum-device-api-framing",
            "reticulum-device-api-pairing-control",
        ] {
            assert!(
                E290_NODE_GRAPH_REQUIRED.contains(&package),
                "permanent E290 node no longer requires {package}"
            );
            assert!(
                !E290_NODE_GRAPH_FORBIDDEN.contains(&package),
                "permanent E290 node still forbids composed {package}"
            );
        }
    }

    #[test]
    fn live_pairing_core_is_composed_only_by_the_permanent_e290_lifecycle() {
        for (label, forbidden) in [
            ("Tracker product", &PRODUCT_GRAPH_FORBIDDEN[..]),
            ("storage HIL", &STORAGE_HIL_GRAPH_FORBIDDEN[..]),
            ("Tracker TX HIL", &TX_HIL_GRAPH_FORBIDDEN[..]),
            (
                "Tracker semantic TX HIL",
                &SEMANTIC_TX_HIL_GRAPH_FORBIDDEN[..],
            ),
            ("E290 semantic HIL", &E290_SEMANTIC_HIL_GRAPH_FORBIDDEN[..]),
        ] {
            assert!(
                forbidden.contains(&"reticulum-device-api-pairing"),
                "{label} no longer forbids the E290 live pairing core"
            );
        }
        assert!(E290_NODE_GRAPH_REQUIRED.contains(&"reticulum-device-api-pairing"));
        assert!(!E290_NODE_GRAPH_FORBIDDEN.contains(&"reticulum-device-api-pairing"));
    }

    #[test]
    fn authenticated_api_node_dependencies_are_composed_only_by_the_permanent_e290_node() {
        for (label, forbidden) in [
            ("Tracker product", &PRODUCT_GRAPH_FORBIDDEN[..]),
            ("storage HIL", &STORAGE_HIL_GRAPH_FORBIDDEN[..]),
            ("Tracker TX HIL", &TX_HIL_GRAPH_FORBIDDEN[..]),
            (
                "Tracker semantic TX HIL",
                &SEMANTIC_TX_HIL_GRAPH_FORBIDDEN[..],
            ),
            ("E290 semantic HIL", &E290_SEMANTIC_HIL_GRAPH_FORBIDDEN[..]),
        ] {
            for package in [
                "reticulum-device-api-handoff",
                "reticulum-device-api-session",
            ] {
                assert!(
                    forbidden.contains(&package),
                    "{label} no longer forbids the authenticated API node package {package}"
                );
            }
        }
        for package in [
            "reticulum-device-api-handoff",
            "reticulum-device-api-session",
        ] {
            assert!(
                E290_NODE_GRAPH_REQUIRED.contains(&package),
                "permanent E290 node no longer requires {package}"
            );
            assert!(
                !E290_NODE_GRAPH_FORBIDDEN.contains(&package),
                "permanent E290 node still forbids composed {package}"
            );
        }
    }

    #[test]
    fn cargo_tree_package_matching_does_not_confuse_prefixed_siblings() {
        let tree = "reticulum-device-api-pairing-control v0.1.0\n\
                    reticulum-device-api-pairing-policy v0.1.0";
        assert!(cargo_tree_contains_package(
            tree,
            "reticulum-device-api-pairing-control"
        ));
        assert!(cargo_tree_contains_package(
            tree,
            "reticulum-device-api-pairing-policy"
        ));
        assert!(!cargo_tree_contains_package(
            tree,
            "reticulum-device-api-pairing"
        ));
    }

    #[test]
    fn storage_hil_boundary_rejects_dependency_feature_and_edge_drift() {
        let root = workspace_root();
        let metadata = storage_hil_metadata_fixture(&root);
        validate_storage_hil_dependency_boundary(&metadata.to_string(), &root).unwrap();

        let mut default_features = metadata.clone();
        default_features["packages"][0]["dependencies"][6]["uses_default_features"] =
            serde_json::Value::Bool(true);
        assert!(
            validate_storage_hil_dependency_boundary(&default_features.to_string(), &root).is_err()
        );

        let mut unreviewed_feature = metadata.clone();
        unreviewed_feature["packages"][0]["dependencies"][3]["features"]
            .as_array_mut()
            .unwrap()
            .push(serde_json::Value::String("radio".to_owned()));
        assert!(
            validate_storage_hil_dependency_boundary(&unreviewed_feature.to_string(), &root)
                .is_err()
        );

        let mut local_feature = metadata.clone();
        local_feature["packages"][0]["dependencies"][7]["features"] =
            serde_json::json!(["unreviewed"]);
        assert!(
            validate_storage_hil_dependency_boundary(&local_feature.to_string(), &root).is_err()
        );

        let mut wrong_path = metadata.clone();
        wrong_path["packages"][0]["dependencies"][9]["path"] =
            serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(validate_storage_hil_dependency_boundary(&wrong_path.to_string(), &root).is_err());

        let mut extra_dependency = metadata.clone();
        extra_dependency["packages"][0]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("heapless", "=0.9.1", None));
        assert!(
            validate_storage_hil_dependency_boundary(&extra_dependency.to_string(), &root).is_err()
        );

        let mut cargo_feature = metadata;
        cargo_feature["packages"][0]["features"]["rf"] = serde_json::json!([]);
        assert!(
            validate_storage_hil_dependency_boundary(&cargo_feature.to_string(), &root).is_err()
        );
    }

    #[test]
    fn tracker_radio_boundary_locks_product_owner_and_hil_facade_shapes() {
        let root = workspace_root();
        let metadata = tracker_radio_metadata_fixture(&root);
        validate_tracker_radio_dependency_boundary(&metadata.to_string(), &root).unwrap();

        let mut wrong_board_path = metadata.clone();
        wrong_board_path["packages"][0]["dependencies"][4]["path"] =
            serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_tracker_radio_dependency_boundary(&wrong_board_path.to_string(), &root)
                .is_err()
        );

        let mut arbitrary_power_feature = metadata.clone();
        arbitrary_power_feature["packages"][0]["features"]["arbitrary-power"] =
            serde_json::json!([]);
        assert!(
            validate_tracker_radio_dependency_boundary(&arbitrary_power_feature.to_string(), &root)
                .is_err()
        );

        let mut wrong_feature_forward = metadata.clone();
        wrong_feature_forward["packages"][1]["features"]["near-field-attenuation-hil"] =
            serde_json::json!(["reticulum-board-heltec-tracker-v2-radio/default"]);
        assert!(
            validate_tracker_radio_dependency_boundary(&wrong_feature_forward.to_string(), &root)
                .is_err()
        );

        let mut extra_facade_edge = metadata;
        extra_facade_edge["packages"][1]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("lora-phy", "=3.0.1", None));
        assert!(
            validate_tracker_radio_dependency_boundary(&extra_facade_edge.to_string(), &root)
                .is_err()
        );
    }

    #[test]
    fn portable_layer_boundary_accepts_generic_dependencies_and_node_rete_adapter() {
        let root = workspace_root();
        let metadata = portable_layers_metadata_fixture(&root);

        validate_portable_layer_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_tx_handoff_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_tx_dispatch_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_radio_tx_dispatch_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_lxmf_wire_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_storage_model_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_storage_journal_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_storage_actor_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_device_api_adapter_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_submission_projector_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_tx_supervisor_dependency_boundary(&metadata.to_string(), &root).unwrap();

        let mut wrong_path = metadata;
        wrong_path["packages"][0]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_portable_layer_dependency_boundary(&wrong_path.to_string(), &root).is_err()
        );
    }

    #[test]
    fn device_api_edge_boundary_locks_framing_pairing_control_handoff_credentials_policy_pairing_and_session_shapes()
     {
        let root = workspace_root();
        let metadata = device_api_edge_metadata_fixture(&root);
        validate_device_api_edge_dependency_boundary(&metadata.to_string(), &root).unwrap();

        let mut framing_dependency = metadata.clone();
        fixture_package_mut(
            &mut framing_dependency,
            "reticulum-device-api-framing",
        )["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("cobs", "=0.3.0", None));
        assert!(
            validate_device_api_edge_dependency_boundary(&framing_dependency.to_string(), &root,)
                .is_err()
        );

        let mut wrong_framing_zeroize = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut wrong_framing_zeroize, "reticulum-device-api-framing"),
            "zeroize",
            None,
        )["req"] = serde_json::json!("=0.0.0");
        assert!(
            validate_device_api_edge_dependency_boundary(
                &wrong_framing_zeroize.to_string(),
                &root,
            )
            .is_err(),
            "framing accepted an unreviewed zeroize version"
        );

        let mut pairing_control_feature = metadata.clone();
        fixture_package_mut(
            &mut pairing_control_feature,
            "reticulum-device-api-pairing-control",
        )["features"]["default"] = serde_json::json!([]);
        assert!(
            validate_device_api_edge_dependency_boundary(
                &pairing_control_feature.to_string(),
                &root,
            )
            .is_err(),
            "pairing control accepted a crate feature"
        );

        for (field, value) in [
            ("req", serde_json::json!("^0.1")),
            (
                "source",
                serde_json::json!("registry+https://github.com/rust-lang/crates.io-index"),
            ),
            ("path", serde_json::json!(root.join("elsewhere"))),
            ("kind", serde_json::json!("dev")),
            ("optional", serde_json::json!(true)),
            ("rename", serde_json::json!("framing")),
            ("target", serde_json::json!("cfg(target_os = \"none\")")),
            ("uses_default_features", serde_json::json!(true)),
            ("features", serde_json::json!(["unreviewed"])),
        ] {
            let mut pairing_control_dependency = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(
                    &mut pairing_control_dependency,
                    "reticulum-device-api-pairing-control",
                ),
                "reticulum-device-api-framing",
                None,
            )[field] = value;
            assert!(
                validate_device_api_edge_dependency_boundary(
                    &pairing_control_dependency.to_string(),
                    &root,
                )
                .is_err(),
                "pairing control accepted framing dependency drift in {field}"
            );
        }

        let mut missing_pairing_control_dependency = metadata.clone();
        fixture_package_mut(
            &mut missing_pairing_control_dependency,
            "reticulum-device-api-pairing-control",
        )["dependencies"] = serde_json::json!([]);
        assert!(
            validate_device_api_edge_dependency_boundary(
                &missing_pairing_control_dependency.to_string(),
                &root,
            )
            .is_err(),
            "pairing control accepted a missing framing dependency"
        );

        let mut wrong_sync = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut wrong_sync, "reticulum-device-api-handoff"),
            "embassy-sync",
            None,
        )["req"] = serde_json::Value::String("=0.7.2".to_owned());
        assert!(
            validate_device_api_edge_dependency_boundary(&wrong_sync.to_string(), &root).is_err()
        );

        let mut wrong_api_path = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut wrong_api_path, "reticulum-device-api-handoff"),
            "reticulum-device-api",
            None,
        )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_device_api_edge_dependency_boundary(&wrong_api_path.to_string(), &root)
                .is_err()
        );

        let mut extra_handoff_dependency = metadata.clone();
        fixture_package_mut(
            &mut extra_handoff_dependency,
            "reticulum-device-api-handoff",
        )["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("esp-hal", "=1.1.1", None));
        assert!(
            validate_device_api_edge_dependency_boundary(
                &extra_handoff_dependency.to_string(),
                &root,
            )
            .is_err()
        );

        for dependency_name in ["subtle", "zeroize"] {
            let mut wrong_credentials_version = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(
                    &mut wrong_credentials_version,
                    "reticulum-device-api-credentials",
                ),
                dependency_name,
                None,
            )["req"] = serde_json::Value::String("=0.0.0".to_owned());
            assert!(
                validate_device_api_edge_dependency_boundary(
                    &wrong_credentials_version.to_string(),
                    &root,
                )
                .is_err(),
                "credentials accepted wrong {dependency_name} version"
            );
        }

        let mut wrong_credentials_api_path = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(
                &mut wrong_credentials_api_path,
                "reticulum-device-api-credentials",
            ),
            "reticulum-device-api",
            None,
        )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_device_api_edge_dependency_boundary(
                &wrong_credentials_api_path.to_string(),
                &root,
            )
            .is_err(),
            "credentials accepted the wrong device-API path"
        );

        for dependency_name in ["reticulum-device-api", "subtle", "zeroize"] {
            let mut credentials_default_features = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(
                    &mut credentials_default_features,
                    "reticulum-device-api-credentials",
                ),
                dependency_name,
                None,
            )["uses_default_features"] = serde_json::Value::Bool(true);
            assert!(
                validate_device_api_edge_dependency_boundary(
                    &credentials_default_features.to_string(),
                    &root,
                )
                .is_err(),
                "credentials accepted {dependency_name} default features"
            );
        }

        let mut credentials_dev_dependency = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(
                &mut credentials_dev_dependency,
                "reticulum-device-api-credentials",
            ),
            "subtle",
            None,
        )["kind"] = serde_json::Value::String("dev".to_owned());
        assert!(
            validate_device_api_edge_dependency_boundary(
                &credentials_dev_dependency.to_string(),
                &root,
            )
            .is_err(),
            "credentials accepted a dev-only dependency"
        );

        let mut credentials_feature = metadata.clone();
        fixture_package_mut(&mut credentials_feature, "reticulum-device-api-credentials")["features"]
            ["std"] = serde_json::json!([]);
        assert!(
            validate_device_api_edge_dependency_boundary(&credentials_feature.to_string(), &root)
                .is_err(),
            "credentials accepted a crate feature"
        );

        for drifted_features in [
            serde_json::json!({}),
            serde_json::json!({ "default": ["unreviewed"] }),
            serde_json::json!({ "default": [], "std": [] }),
        ] {
            let mut pairing_feature = metadata.clone();
            fixture_package_mut(&mut pairing_feature, "reticulum-device-api-pairing-policy")["features"] =
                drifted_features;
            assert!(
                validate_device_api_edge_dependency_boundary(&pairing_feature.to_string(), &root,)
                    .is_err(),
                "pairing policy accepted feature drift"
            );
        }

        for (field, value) in [
            ("req", serde_json::json!("^0.1")),
            (
                "source",
                serde_json::json!("registry+https://github.com/rust-lang/crates.io-index"),
            ),
            ("path", serde_json::json!(root.join("elsewhere"))),
            ("kind", serde_json::json!("dev")),
            ("optional", serde_json::json!(true)),
            ("rename", serde_json::json!("credentials")),
            ("target", serde_json::json!("cfg(target_os = \"none\")")),
            ("uses_default_features", serde_json::json!(true)),
            ("features", serde_json::json!(["unreviewed"])),
        ] {
            let mut pairing_dependency = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(
                    &mut pairing_dependency,
                    "reticulum-device-api-pairing-policy",
                ),
                "reticulum-device-api-credentials",
                None,
            )[field] = value;
            assert!(
                validate_device_api_edge_dependency_boundary(
                    &pairing_dependency.to_string(),
                    &root,
                )
                .is_err(),
                "pairing policy accepted credential dependency drift in {field}"
            );
        }

        let mut extra_pairing_dependency = metadata.clone();
        fixture_package_mut(
            &mut extra_pairing_dependency,
            "reticulum-device-api-pairing-policy",
        )["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("esp-hal", "=1.1.1", None));
        assert!(
            validate_device_api_edge_dependency_boundary(
                &extra_pairing_dependency.to_string(),
                &root,
            )
            .is_err(),
            "pairing policy accepted an extra dependency"
        );

        let mut duplicate_pairing_package = metadata.clone();
        let duplicate = fixture_package_mut(
            &mut duplicate_pairing_package,
            "reticulum-device-api-pairing-policy",
        )
        .clone();
        duplicate_pairing_package["packages"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(
            validate_device_api_edge_dependency_boundary(
                &duplicate_pairing_package.to_string(),
                &root,
            )
            .is_err(),
            "pairing policy accepted a duplicate local package"
        );

        for (dependency_name, kind) in [
            ("hmac", None),
            ("sha2", None),
            ("zeroize", None),
            ("hex", Some("dev")),
        ] {
            let mut wrong_pairing_protocol_version = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(
                    &mut wrong_pairing_protocol_version,
                    "reticulum-device-api-pairing",
                ),
                dependency_name,
                kind,
            )["req"] = serde_json::json!("=0.0.0");
            assert!(
                validate_device_api_edge_dependency_boundary(
                    &wrong_pairing_protocol_version.to_string(),
                    &root,
                )
                .is_err(),
                "pairing protocol accepted wrong {dependency_name} version"
            );
        }

        for dependency_name in [
            "reticulum-device-api-credentials",
            "reticulum-device-api-framing",
        ] {
            let mut wrong_pairing_protocol_path = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(
                    &mut wrong_pairing_protocol_path,
                    "reticulum-device-api-pairing",
                ),
                dependency_name,
                None,
            )["path"] = serde_json::json!(root.join("elsewhere"));
            assert!(
                validate_device_api_edge_dependency_boundary(
                    &wrong_pairing_protocol_path.to_string(),
                    &root,
                )
                .is_err(),
                "pairing protocol accepted wrong {dependency_name} path"
            );
        }

        let mut pairing_protocol_normal_hex = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(
                &mut pairing_protocol_normal_hex,
                "reticulum-device-api-pairing",
            ),
            "hex",
            Some("dev"),
        )["kind"] = serde_json::Value::Null;
        assert!(
            validate_device_api_edge_dependency_boundary(
                &pairing_protocol_normal_hex.to_string(),
                &root,
            )
            .is_err(),
            "pairing protocol accepted hex as a normal dependency"
        );

        let mut pairing_protocol_feature = metadata.clone();
        fixture_package_mut(
            &mut pairing_protocol_feature,
            "reticulum-device-api-pairing",
        )["features"]["std"] = serde_json::json!([]);
        assert!(
            validate_device_api_edge_dependency_boundary(
                &pairing_protocol_feature.to_string(),
                &root,
            )
            .is_err(),
            "pairing protocol accepted a crate feature"
        );

        let mut extra_pairing_protocol_dependency = metadata.clone();
        fixture_package_mut(
            &mut extra_pairing_protocol_dependency,
            "reticulum-device-api-pairing",
        )["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("rand_core", "=0.6.4", None));
        assert!(
            validate_device_api_edge_dependency_boundary(
                &extra_pairing_protocol_dependency.to_string(),
                &root,
            )
            .is_err(),
            "pairing protocol accepted an extra dependency"
        );

        for (dependency_name, kind) in [
            ("hkdf", None),
            ("hmac", None),
            ("rand_core", None),
            ("sha2", None),
            ("zeroize", None),
            ("hex", Some("dev")),
        ] {
            let mut wrong_version = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut wrong_version, "reticulum-device-api-session"),
                dependency_name,
                kind,
            )["req"] = serde_json::Value::String("=0.0.0".to_owned());
            assert!(
                validate_device_api_edge_dependency_boundary(&wrong_version.to_string(), &root)
                    .is_err(),
                "session accepted wrong {dependency_name} version"
            );
        }

        for (dependency_name, kind) in [
            ("reticulum-device-api", None),
            ("reticulum-device-api-credentials", None),
            ("reticulum-device-api-framing", None),
            ("reticulum-device-api-handoff", None),
            ("reticulum-device-api-adapter", Some("dev")),
            ("reticulum-storage-model", Some("dev")),
        ] {
            let mut wrong_session_path = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut wrong_session_path, "reticulum-device-api-session"),
                dependency_name,
                kind,
            )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_device_api_edge_dependency_boundary(
                    &wrong_session_path.to_string(),
                    &root,
                )
                .is_err(),
                "session accepted wrong {dependency_name} path"
            );
        }

        let mut normal_hex = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut normal_hex, "reticulum-device-api-session"),
            "hex",
            Some("dev"),
        )["kind"] = serde_json::Value::Null;
        assert!(
            validate_device_api_edge_dependency_boundary(&normal_hex.to_string(), &root).is_err(),
            "session accepted hex as a normal dependency"
        );

        let mut default_hkdf = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut default_hkdf, "reticulum-device-api-session"),
            "hkdf",
            None,
        )["uses_default_features"] = serde_json::Value::Bool(true);
        assert!(
            validate_device_api_edge_dependency_boundary(&default_hkdf.to_string(), &root).is_err(),
            "session accepted hkdf default features"
        );

        for (dependency_name, kind) in [
            ("reticulum-device-api-credentials", None),
            ("reticulum-device-api-framing", None),
            ("reticulum-device-api-handoff", None),
            ("reticulum-device-api-adapter", Some("dev")),
            ("reticulum-storage-model", Some("dev")),
        ] {
            let mut disabled_defaults = metadata.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut disabled_defaults, "reticulum-device-api-session"),
                dependency_name,
                kind,
            )["uses_default_features"] = serde_json::Value::Bool(false);
            assert!(
                validate_device_api_edge_dependency_boundary(
                    &disabled_defaults.to_string(),
                    &root,
                )
                .is_err(),
                "session accepted {dependency_name} default-feature drift"
            );
        }

        let mut missing_adapter_feature = metadata.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut missing_adapter_feature, "reticulum-device-api-session"),
            "reticulum-device-api-adapter",
            Some("dev"),
        )["features"] = serde_json::json!([]);
        assert!(
            validate_device_api_edge_dependency_boundary(
                &missing_adapter_feature.to_string(),
                &root,
            )
            .is_err(),
            "session accepted an adapter fixture without experimental-rns-data"
        );

        let mut session_feature = metadata.clone();
        fixture_package_mut(&mut session_feature, "reticulum-device-api-session")["features"]["std"] =
            serde_json::json!([]);
        assert!(
            validate_device_api_edge_dependency_boundary(&session_feature.to_string(), &root)
                .is_err()
        );

        let mut extra_session_dependency = metadata;
        fixture_package_mut(
            &mut extra_session_dependency,
            "reticulum-device-api-session",
        )["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("esp-hal", "=1.1.1", None));
        assert!(
            validate_device_api_edge_dependency_boundary(
                &extra_session_dependency.to_string(),
                &root,
            )
            .is_err()
        );
    }

    #[test]
    fn tx_handoff_boundary_rejects_wrong_paths_versions_and_extra_dependencies() {
        let root = workspace_root();

        let mut wrong_path = portable_layers_metadata_fixture(&root);
        wrong_path["packages"][2]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(validate_tx_handoff_dependency_boundary(&wrong_path.to_string(), &root).is_err());

        let mut wrong_node_path = portable_layers_metadata_fixture(&root);
        wrong_node_path["packages"][2]["dependencies"][0]["path"] =
            serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_tx_handoff_dependency_boundary(&wrong_node_path.to_string(), &root).is_err()
        );

        let mut wrong_embassy = portable_layers_metadata_fixture(&root);
        wrong_embassy["packages"][2]["dependencies"][1]["req"] =
            serde_json::Value::String("=0.7.2".to_owned());
        assert!(
            validate_tx_handoff_dependency_boundary(&wrong_embassy.to_string(), &root).is_err()
        );

        let mut extra = portable_layers_metadata_fixture(&root);
        extra["packages"][2]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture(
                "reticulum-radio-interface",
                "*",
                None,
            ));
        assert!(validate_tx_handoff_dependency_boundary(&extra.to_string(), &root).is_err());

        let mut build = portable_layers_metadata_fixture(&root);
        let mut build_dependency = handoff_dependency_fixture("cc", "=1.0.0", Some("dev"));
        build_dependency["kind"] = serde_json::Value::String("build".to_owned());
        build["packages"][2]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(build_dependency);
        assert!(validate_tx_handoff_dependency_boundary(&build.to_string(), &root).is_err());
    }

    #[test]
    fn tx_dispatch_boundary_rejects_unreviewed_dependencies_features_and_edge_shapes() {
        let root = workspace_root();

        let mut wrong_manifest = portable_layers_metadata_fixture(&root);
        wrong_manifest["packages"][3]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_tx_dispatch_dependency_boundary(&wrong_manifest.to_string(), &root).is_err()
        );

        for dependency_index in 0..=1 {
            let mut wrong_path = portable_layers_metadata_fixture(&root);
            wrong_path["packages"][3]["dependencies"][dependency_index]["path"] =
                serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_tx_dispatch_dependency_boundary(&wrong_path.to_string(), &root).is_err(),
                "local dependency {dependency_index} accepted the wrong path"
            );

            let mut default_features = portable_layers_metadata_fixture(&root);
            default_features["packages"][3]["dependencies"][dependency_index]["uses_default_features"] =
                serde_json::Value::Bool(true);
            assert!(
                validate_tx_dispatch_dependency_boundary(&default_features.to_string(), &root)
                    .is_err(),
                "local dependency {dependency_index} accepted default features"
            );
        }

        for (field, value) in [
            ("optional", serde_json::Value::Bool(true)),
            (
                "rename",
                serde_json::Value::String("renamed-node".to_owned()),
            ),
            (
                "target",
                serde_json::Value::String("cfg(target_os = \"none\")".to_owned()),
            ),
        ] {
            let mut changed = portable_layers_metadata_fixture(&root);
            changed["packages"][3]["dependencies"][0][field] = value;
            assert!(
                validate_tx_dispatch_dependency_boundary(&changed.to_string(), &root).is_err(),
                "local dependency accepted changed {field}"
            );
        }

        let mut local_feature = portable_layers_metadata_fixture(&root);
        local_feature["packages"][3]["dependencies"][1]["features"] =
            serde_json::json!(["unreviewed"]);
        assert!(
            validate_tx_dispatch_dependency_boundary(&local_feature.to_string(), &root).is_err()
        );

        let mut wrong_embassy = portable_layers_metadata_fixture(&root);
        wrong_embassy["packages"][3]["dependencies"][2]["req"] =
            serde_json::Value::String("=0.7.2".to_owned());
        assert!(
            validate_tx_dispatch_dependency_boundary(&wrong_embassy.to_string(), &root).is_err()
        );

        let mut embassy_defaults = portable_layers_metadata_fixture(&root);
        embassy_defaults["packages"][3]["dependencies"][2]["uses_default_features"] =
            serde_json::Value::Bool(true);
        assert!(
            validate_tx_dispatch_dependency_boundary(&embassy_defaults.to_string(), &root).is_err()
        );

        let mut extra_normal = portable_layers_metadata_fixture(&root);
        extra_normal["packages"][3]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_path_dependency_fixture(
                "reticulum-radio-interface",
                "*",
                &root.join("crates/radio-interface"),
                None,
            ));
        assert!(
            validate_tx_dispatch_dependency_boundary(&extra_normal.to_string(), &root).is_err()
        );

        let mut wrong_rand = portable_layers_metadata_fixture(&root);
        wrong_rand["packages"][3]["dependencies"][3]["req"] =
            serde_json::Value::String("=0.6.3".to_owned());
        assert!(validate_tx_dispatch_dependency_boundary(&wrong_rand.to_string(), &root).is_err());

        let mut wrong_sha2 = portable_layers_metadata_fixture(&root);
        wrong_sha2["packages"][3]["dependencies"][4]["req"] =
            serde_json::Value::String("=0.10.8".to_owned());
        assert!(validate_tx_dispatch_dependency_boundary(&wrong_sha2.to_string(), &root).is_err());

        let mut sha2_defaults = portable_layers_metadata_fixture(&root);
        sha2_defaults["packages"][3]["dependencies"][4]["uses_default_features"] =
            serde_json::Value::Bool(true);
        assert!(
            validate_tx_dispatch_dependency_boundary(&sha2_defaults.to_string(), &root).is_err()
        );

        let mut sha2_feature = portable_layers_metadata_fixture(&root);
        sha2_feature["packages"][3]["dependencies"][4]["features"] =
            serde_json::json!(["unreviewed"]);
        assert!(
            validate_tx_dispatch_dependency_boundary(&sha2_feature.to_string(), &root).is_err()
        );

        let mut wrong_dev = portable_layers_metadata_fixture(&root);
        wrong_dev["packages"][3]["dependencies"][5]["req"] =
            serde_json::Value::String("=0.1.1".to_owned());
        assert!(validate_tx_dispatch_dependency_boundary(&wrong_dev.to_string(), &root).is_err());

        let mut wrong_dev_defaults = portable_layers_metadata_fixture(&root);
        wrong_dev_defaults["packages"][3]["dependencies"][6]["uses_default_features"] =
            serde_json::Value::Bool(false);
        assert!(
            validate_tx_dispatch_dependency_boundary(&wrong_dev_defaults.to_string(), &root)
                .is_err()
        );

        let mut build = portable_layers_metadata_fixture(&root);
        let mut build_dependency = handoff_dependency_fixture("cc", "=1.0.0", Some("dev"));
        build_dependency["kind"] = serde_json::Value::String("build".to_owned());
        build["packages"][3]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(build_dependency);
        assert!(validate_tx_dispatch_dependency_boundary(&build.to_string(), &root).is_err());

        let mut extra_feature = portable_layers_metadata_fixture(&root);
        extra_feature["packages"][3]["features"]["rf"] = serde_json::json!([]);
        assert!(
            validate_tx_dispatch_dependency_boundary(&extra_feature.to_string(), &root).is_err()
        );
    }

    #[test]
    fn radio_interface_boundary_rejects_dependency_and_feature_drift() {
        let root = workspace_root();
        let baseline = portable_layers_metadata_fixture(&root);
        validate_radio_interface_dependency_boundary(&baseline.to_string(), &root).unwrap();

        let mut wrong_manifest = baseline.clone();
        fixture_package_mut(&mut wrong_manifest, "reticulum-radio-interface")["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_radio_interface_dependency_boundary(&wrong_manifest.to_string(), &root)
                .is_err()
        );

        let mut duplicate = baseline.clone();
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(radio_interface_package_fixture(&root));
        assert!(
            validate_radio_interface_dependency_boundary(&duplicate.to_string(), &root).is_err()
        );

        let mut extra_feature = baseline.clone();
        fixture_package_mut(&mut extra_feature, "reticulum-radio-interface")["features"]["default"] =
            serde_json::json!([]);
        assert!(
            validate_radio_interface_dependency_boundary(&extra_feature.to_string(), &root)
                .is_err()
        );

        for (name, kind) in [
            ("lora-modulation", None),
            ("reticulum-rns-conformance", None),
            ("embassy-sync", Some("dev")),
        ] {
            let mut dependency_feature = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut dependency_feature, "reticulum-radio-interface"),
                name,
                kind,
            )["features"] = serde_json::json!(["unreviewed"]);
            assert!(
                validate_radio_interface_dependency_boundary(
                    &dependency_feature.to_string(),
                    &root,
                )
                .is_err(),
                "dependency {name} accepted an unreviewed feature"
            );

            for (field, value) in [
                ("optional", serde_json::Value::Bool(true)),
                (
                    "rename",
                    serde_json::Value::String("renamed-dependency".to_owned()),
                ),
                (
                    "target",
                    serde_json::Value::String("cfg(target_os = \"none\")".to_owned()),
                ),
            ] {
                let mut changed = baseline.clone();
                fixture_dependency_mut(
                    fixture_package_mut(&mut changed, "reticulum-radio-interface"),
                    name,
                    kind,
                )[field] = value;
                assert!(
                    validate_radio_interface_dependency_boundary(&changed.to_string(), &root)
                        .is_err(),
                    "dependency {name} accepted changed {field}"
                );
            }

            let mut changed_defaults = baseline.clone();
            let dependency = fixture_dependency_mut(
                fixture_package_mut(&mut changed_defaults, "reticulum-radio-interface"),
                name,
                kind,
            );
            let reviewed_default = name == "reticulum-rns-conformance";
            dependency["uses_default_features"] = serde_json::Value::Bool(!reviewed_default);
            assert!(
                validate_radio_interface_dependency_boundary(&changed_defaults.to_string(), &root,)
                    .is_err(),
                "dependency {name} accepted changed default-feature behavior"
            );
        }

        for (name, kind, wrong_requirement) in [
            ("lora-modulation", None, "=0.1.4"),
            ("reticulum-rns-conformance", None, "=0.1.0"),
            ("embassy-sync", Some("dev"), "=0.7.2"),
        ] {
            let mut wrong_pin = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut wrong_pin, "reticulum-radio-interface"),
                name,
                kind,
            )["req"] = serde_json::Value::String(wrong_requirement.to_owned());
            assert!(
                validate_radio_interface_dependency_boundary(&wrong_pin.to_string(), &root)
                    .is_err(),
                "dependency {name} accepted requirement {wrong_requirement}"
            );
        }

        for (name, kind) in [("lora-modulation", None), ("embassy-sync", Some("dev"))] {
            let mut registry_path = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut registry_path, "reticulum-radio-interface"),
                name,
                kind,
            )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_radio_interface_dependency_boundary(&registry_path.to_string(), &root)
                    .is_err(),
                "registry dependency {name} accepted a local path"
            );
        }

        let mut wrong_local_path = baseline.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut wrong_local_path, "reticulum-radio-interface"),
            "reticulum-rns-conformance",
            None,
        )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_radio_interface_dependency_boundary(&wrong_local_path.to_string(), &root)
                .is_err()
        );

        let mut wrong_local_source = baseline.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut wrong_local_source, "reticulum-radio-interface"),
            "reticulum-rns-conformance",
            None,
        )["source"] = serde_json::Value::String(
            "registry+https://github.com/rust-lang/crates.io-index".to_owned(),
        );
        assert!(
            validate_radio_interface_dependency_boundary(&wrong_local_source.to_string(), &root)
                .is_err()
        );

        let mut wrong_kind = baseline.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut wrong_kind, "reticulum-radio-interface"),
            "embassy-sync",
            Some("dev"),
        )["kind"] = serde_json::Value::Null;
        assert!(
            validate_radio_interface_dependency_boundary(&wrong_kind.to_string(), &root).is_err()
        );

        let mut extra_dependency = baseline.clone();
        fixture_package_mut(&mut extra_dependency, "reticulum-radio-interface")["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("heapless", "=0.9.1", None));
        assert!(
            validate_radio_interface_dependency_boundary(&extra_dependency.to_string(), &root)
                .is_err()
        );

        let mut build = baseline;
        let mut build_dependency = handoff_dependency_fixture("cc", "=1.2.0", Some("dev"));
        build_dependency["kind"] = serde_json::Value::String("build".to_owned());
        fixture_package_mut(&mut build, "reticulum-radio-interface")["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(build_dependency);
        assert!(validate_radio_interface_dependency_boundary(&build.to_string(), &root).is_err());
    }

    #[test]
    fn e290_board_facts_boundary_rejects_platform_and_edge_drift() {
        let root = workspace_root();
        let baseline = portable_layers_metadata_fixture(&root);
        validate_e290_board_facts_dependency_boundary(&baseline.to_string(), &root).unwrap();

        let mut wrong_manifest = baseline.clone();
        fixture_package_mut(
            &mut wrong_manifest,
            "reticulum-board-heltec-vision-master-e290",
        )["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_e290_board_facts_dependency_boundary(&wrong_manifest.to_string(), &root)
                .is_err()
        );

        let mut duplicate = baseline.clone();
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(e290_board_facts_package_fixture(&root));
        assert!(
            validate_e290_board_facts_dependency_boundary(&duplicate.to_string(), &root).is_err()
        );

        let mut extra_feature = baseline.clone();
        fixture_package_mut(
            &mut extra_feature,
            "reticulum-board-heltec-vision-master-e290",
        )["features"]["default"] = serde_json::json!([]);
        assert!(
            validate_e290_board_facts_dependency_boundary(&extra_feature.to_string(), &root)
                .is_err()
        );

        for (field, value) in [
            ("optional", serde_json::Value::Bool(true)),
            (
                "rename",
                serde_json::Value::String("renamed-interface".to_owned()),
            ),
            (
                "target",
                serde_json::Value::String("cfg(target_os = \"none\")".to_owned()),
            ),
            ("uses_default_features", serde_json::Value::Bool(true)),
            ("features", serde_json::json!(["unreviewed"])),
            ("req", serde_json::Value::String("=0.1.0".to_owned())),
            (
                "path",
                serde_json::Value::String(root.join("elsewhere").display().to_string()),
            ),
            (
                "source",
                serde_json::Value::String(CRATES_IO_SOURCE.to_owned()),
            ),
            ("kind", serde_json::Value::String("dev".to_owned())),
        ] {
            let mut changed = baseline.clone();
            fixture_dependency_mut(
                fixture_package_mut(&mut changed, "reticulum-board-heltec-vision-master-e290"),
                "reticulum-radio-interface",
                None,
            )[field] = value;
            assert!(
                validate_e290_board_facts_dependency_boundary(&changed.to_string(), &root).is_err(),
                "E290 board-facts edge accepted changed {field}"
            );
        }

        let mut extra_dependency = baseline;
        fixture_package_mut(
            &mut extra_dependency,
            "reticulum-board-heltec-vision-master-e290",
        )["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("esp-hal", "=1.1.1", None));
        assert!(
            validate_e290_board_facts_dependency_boundary(&extra_dependency.to_string(), &root)
                .is_err()
        );
    }

    #[test]
    fn shared_lora_phy_and_e290_radio_boundaries_reject_edge_drift() {
        let root = workspace_root();
        let baseline = portable_layers_metadata_fixture(&root);
        validate_lora_phy_radio_dependency_boundary(&baseline.to_string(), &root).unwrap();
        validate_e290_radio_dependency_boundary(&baseline.to_string(), &root).unwrap();

        let mut shared_extra = baseline.clone();
        fixture_package_mut(&mut shared_extra, "reticulum-radio-lora-phy")["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("esp-hal", "=1.1.1", None));
        assert!(
            validate_lora_phy_radio_dependency_boundary(&shared_extra.to_string(), &root).is_err()
        );

        let mut shared_wrong_path = baseline.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut shared_wrong_path, "reticulum-radio-lora-phy"),
            "reticulum-radio-interface",
            None,
        )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_lora_phy_radio_dependency_boundary(&shared_wrong_path.to_string(), &root)
                .is_err()
        );

        let mut e290_feature = baseline.clone();
        fixture_package_mut(
            &mut e290_feature,
            "reticulum-board-heltec-vision-master-e290-radio",
        )["features"]["arbitrary-power"] = serde_json::json!([]);
        assert!(validate_e290_radio_dependency_boundary(&e290_feature.to_string(), &root).is_err());

        let mut e290_wrong_driver = baseline;
        fixture_dependency_mut(
            fixture_package_mut(
                &mut e290_wrong_driver,
                "reticulum-board-heltec-vision-master-e290-radio",
            ),
            "lora-phy",
            None,
        )["uses_default_features"] = serde_json::Value::Bool(true);
        assert!(
            validate_e290_radio_dependency_boundary(&e290_wrong_driver.to_string(), &root).is_err()
        );
    }

    #[test]
    fn radio_tx_dispatch_boundary_rejects_platform_storage_and_edge_drift() {
        let root = workspace_root();

        let mut wrong_manifest = portable_layers_metadata_fixture(&root);
        fixture_package_mut(&mut wrong_manifest, "reticulum-radio-tx-dispatch")["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_radio_tx_dispatch_dependency_boundary(&wrong_manifest.to_string(), &root)
                .is_err()
        );

        let mut duplicate = portable_layers_metadata_fixture(&root);
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(radio_tx_dispatch_package_fixture(&root));
        assert!(
            validate_radio_tx_dispatch_dependency_boundary(&duplicate.to_string(), &root).is_err()
        );

        for name in [
            "reticulum-interface-router",
            "reticulum-node-core",
            "reticulum-radio-interface",
            "reticulum-tx-handoff",
        ] {
            let mut wrong_path = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut wrong_path, "reticulum-radio-tx-dispatch"),
                name,
                None,
            )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_radio_tx_dispatch_dependency_boundary(&wrong_path.to_string(), &root)
                    .is_err(),
                "local dependency {name} accepted the wrong path"
            );
        }

        for (name, kind) in [
            ("embassy-futures", Some("dev")),
            ("embassy-sync", None),
            ("embassy-time", None),
            ("rand_core", None),
            ("reticulum-interface-router", None),
            ("reticulum-node-core", None),
            ("reticulum-radio-interface", None),
            ("reticulum-tx-handoff", None),
            ("static_cell", Some("dev")),
        ] {
            let mut dependency_feature = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut dependency_feature, "reticulum-radio-tx-dispatch"),
                name,
                kind,
            )["features"] = serde_json::json!(["unreviewed"]);
            assert!(
                validate_radio_tx_dispatch_dependency_boundary(
                    &dependency_feature.to_string(),
                    &root,
                )
                .is_err(),
                "dependency {name} accepted an unreviewed feature"
            );

            for (field, value) in [
                ("optional", serde_json::Value::Bool(true)),
                (
                    "rename",
                    serde_json::Value::String("renamed-dependency".to_owned()),
                ),
                (
                    "target",
                    serde_json::Value::String("cfg(target_os = \"none\")".to_owned()),
                ),
            ] {
                let mut changed = portable_layers_metadata_fixture(&root);
                fixture_dependency_mut(
                    fixture_package_mut(&mut changed, "reticulum-radio-tx-dispatch"),
                    name,
                    kind,
                )[field] = value;
                assert!(
                    validate_radio_tx_dispatch_dependency_boundary(&changed.to_string(), &root)
                        .is_err(),
                    "dependency {name} accepted changed {field}"
                );
            }
        }

        let mut futures_promoted_to_normal = portable_layers_metadata_fixture(&root);
        fixture_dependency_mut(
            fixture_package_mut(
                &mut futures_promoted_to_normal,
                "reticulum-radio-tx-dispatch",
            ),
            "embassy-futures",
            Some("dev"),
        )["kind"] = serde_json::Value::Null;
        assert!(
            validate_radio_tx_dispatch_dependency_boundary(
                &futures_promoted_to_normal.to_string(),
                &root,
            )
            .is_err()
        );

        let mut futures_defaults = portable_layers_metadata_fixture(&root);
        fixture_dependency_mut(
            fixture_package_mut(&mut futures_defaults, "reticulum-radio-tx-dispatch"),
            "embassy-futures",
            Some("dev"),
        )["uses_default_features"] = serde_json::Value::Bool(true);
        assert!(
            validate_radio_tx_dispatch_dependency_boundary(&futures_defaults.to_string(), &root)
                .is_err()
        );

        for (name, path) in [
            (
                "reticulum-board-heltec-tracker-v2-radio",
                "crates/board-heltec-tracker-v2-radio",
            ),
            ("embedded-hal", "crates/forbidden-embedded-hal"),
            ("reticulum-storage-model", "crates/storage-model"),
            (
                "reticulum-submission-projector",
                "crates/submission-projector",
            ),
            ("reticulum-device-api", "crates/device-api"),
        ] {
            let mut extra = portable_layers_metadata_fixture(&root);
            fixture_package_mut(&mut extra, "reticulum-radio-tx-dispatch")["dependencies"]
                .as_array_mut()
                .unwrap()
                .push(handoff_path_dependency_fixture(
                    name,
                    "*",
                    &root.join(path),
                    None,
                ));
            assert!(
                validate_radio_tx_dispatch_dependency_boundary(&extra.to_string(), &root).is_err(),
                "forbidden dependency {name} was accepted"
            );
        }

        let mut build = portable_layers_metadata_fixture(&root);
        let mut build_dependency = handoff_dependency_fixture("cc", "=1.0.0", Some("dev"));
        build_dependency["kind"] = serde_json::Value::String("build".to_owned());
        fixture_package_mut(&mut build, "reticulum-radio-tx-dispatch")["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(build_dependency);
        assert!(validate_radio_tx_dispatch_dependency_boundary(&build.to_string(), &root).is_err());

        let mut extra_feature = portable_layers_metadata_fixture(&root);
        fixture_package_mut(&mut extra_feature, "reticulum-radio-tx-dispatch")["features"]["board"] =
            serde_json::json!([]);
        assert!(
            validate_radio_tx_dispatch_dependency_boundary(&extra_feature.to_string(), &root)
                .is_err()
        );
    }

    #[test]
    fn lxmf_wire_boundary_allows_only_exact_crypto_and_host_test_edges() {
        let root = workspace_root();
        let baseline = serde_json::json!({
            "packages": [lxmf_wire_package_fixture(&root)],
        });
        validate_lxmf_wire_dependency_boundary(&baseline.to_string(), &root).unwrap();

        let mut wrong_manifest = baseline.clone();
        fixture_package_mut(&mut wrong_manifest, "reticulum-lxmf-wire")["manifest_path"] =
            serde_json::json!(root.join("crates/lookalike-lxmf-wire/Cargo.toml"));
        assert!(
            validate_lxmf_wire_dependency_boundary(&wrong_manifest.to_string(), &root).is_err()
        );

        let mut duplicate = baseline.clone();
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(lxmf_wire_package_fixture(&root));
        assert!(validate_lxmf_wire_dependency_boundary(&duplicate.to_string(), &root).is_err());

        let mut extra_feature = baseline.clone();
        fixture_package_mut(&mut extra_feature, "reticulum-lxmf-wire")["features"]["std"] =
            serde_json::json!([]);
        assert!(validate_lxmf_wire_dependency_boundary(&extra_feature.to_string(), &root).is_err());

        let mut build = baseline.clone();
        let mut build_dependency = handoff_dependency_fixture("cc", "=1.0.0", None);
        build_dependency["kind"] = serde_json::json!("build");
        fixture_package_mut(&mut build, "reticulum-lxmf-wire")["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(build_dependency);
        assert!(validate_lxmf_wire_dependency_boundary(&build.to_string(), &root).is_err());

        for (name, kind, reviewed_defaults) in [
            ("curve25519-dalek", None, false),
            ("ed25519-dalek", None, false),
            ("hkdf", None, false),
            ("sha2", None, false),
            ("subtle", None, false),
            ("hex", Some("dev"), true),
            ("serde", Some("dev"), true),
            ("serde_json", Some("dev"), true),
        ] {
            for (field, value) in [
                ("req", serde_json::json!("=0.0.0")),
                (
                    "source",
                    serde_json::json!("registry+https://example.invalid/index"),
                ),
                (
                    "path",
                    serde_json::json!(root.join("crates/unreviewed-dependency")),
                ),
                ("optional", serde_json::json!(true)),
                ("rename", serde_json::json!("renamed-dependency")),
                ("target", serde_json::json!("cfg(target_os = \"none\")")),
                (
                    "uses_default_features",
                    serde_json::json!(!reviewed_defaults),
                ),
                ("features", serde_json::json!(["std"])),
            ] {
                let mut changed = baseline.clone();
                fixture_dependency_mut(
                    fixture_package_mut(&mut changed, "reticulum-lxmf-wire"),
                    name,
                    kind,
                )[field] = value;
                assert!(
                    validate_lxmf_wire_dependency_boundary(&changed.to_string(), &root).is_err(),
                    "dependency {name} accepted changed {field}"
                );
            }
        }

        let mut normal_promoted_to_dev = baseline.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut normal_promoted_to_dev, "reticulum-lxmf-wire"),
            "sha2",
            None,
        )["kind"] = serde_json::json!("dev");
        assert!(
            validate_lxmf_wire_dependency_boundary(&normal_promoted_to_dev.to_string(), &root)
                .is_err()
        );

        let mut dev_promoted_to_normal = baseline.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut dev_promoted_to_normal, "reticulum-lxmf-wire"),
            "hex",
            Some("dev"),
        )["kind"] = serde_json::Value::Null;
        assert!(
            validate_lxmf_wire_dependency_boundary(&dev_promoted_to_normal.to_string(), &root)
                .is_err()
        );
    }

    #[test]
    fn radio_tx_dispatch_resolved_closure_is_an_exact_identity_and_feature_set() {
        let root = workspace_root();
        let (baseline_metadata, baseline_tree) = radio_tx_dispatch_reviewed_closure_fixture(&root);
        validate_radio_tx_dispatch_resolved_closure(
            &baseline_metadata.to_string(),
            &baseline_tree,
            &root,
        )
        .unwrap();

        for (name, version) in [
            ("embassy-stm32", "0.4.0"),
            ("nrf52840-hal", "0.19.0"),
            ("rp2040-hal", "0.11.0"),
            ("cc1101", "0.1.0"),
            ("arbitrary-registry-wrapper", "7.3.1"),
            ("lora-phy", "3.0.1"),
            ("esp-hal", "1.0.0"),
        ] {
            let mut metadata = baseline_metadata.clone();
            let mut tree = baseline_tree.clone();
            add_registry_closure_fixture_package(&mut metadata, &mut tree, &root, name, version);
            let error =
                validate_radio_tx_dispatch_resolved_closure(&metadata.to_string(), &tree, &root)
                    .unwrap_err();
            assert!(error.contains(name), "registry package {name}: {error}");
        }

        for (name, relative_manifest) in [
            (
                "reticulum-board-heltec-tracker-v2-radio",
                "crates/board-heltec-tracker-v2-radio/Cargo.toml",
            ),
            ("reticulum-storage-actor", "crates/storage-actor/Cargo.toml"),
            ("reticulum-device-api", "crates/device-api/Cargo.toml"),
            (
                "reticulum-submission-projector",
                "crates/submission-projector/Cargo.toml",
            ),
            (
                "reticulum-heltec-tracker-v2",
                "firmware/heltec-tracker-v2/Cargo.toml",
            ),
            ("reticulum-tx-dispatch", "crates/tx-dispatch/Cargo.toml"),
            ("reticulum-tx-supervisor", "crates/tx-supervisor/Cargo.toml"),
            (
                "innocently-named-local-wrapper",
                "crates/innocently-named-local-wrapper/Cargo.toml",
            ),
            ("renamed-storage-wrapper", "crates/storage-actor/Cargo.toml"),
        ] {
            let mut metadata = baseline_metadata.clone();
            let mut tree = baseline_tree.clone();
            add_local_closure_fixture_package(
                &mut metadata,
                &mut tree,
                &root,
                name,
                "0.1.0",
                relative_manifest,
            );
            let error =
                validate_radio_tx_dispatch_resolved_closure(&metadata.to_string(), &tree, &root)
                    .unwrap_err();
            assert!(error.contains(name), "local package {name}: {error}");
        }

        let mut wrong_version = baseline_metadata.clone();
        let embassy_sync = wrong_version["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|package| package["name"].as_str() == Some("embassy-sync"))
            .unwrap();
        embassy_sync["version"] = serde_json::Value::String("0.8.1".to_owned());
        let wrong_version_tree =
            baseline_tree.replace("embassy-sync v0.8.0\t", "embassy-sync v0.8.1\t");
        assert!(
            validate_radio_tx_dispatch_resolved_closure(
                &wrong_version.to_string(),
                &wrong_version_tree,
                &root,
            )
            .is_err()
        );

        let mut wrong_source = baseline_metadata.clone();
        let embassy_sync = wrong_source["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|package| package["name"].as_str() == Some("embassy-sync"))
            .unwrap();
        embassy_sync["source"] =
            serde_json::Value::String("registry+https://example.invalid/index".to_owned());
        assert!(
            validate_radio_tx_dispatch_resolved_closure(
                &wrong_source.to_string(),
                &baseline_tree,
                &root,
            )
            .is_err()
        );

        let mut wrong_path = baseline_metadata.clone();
        let node_core = wrong_path["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|package| package["name"].as_str() == Some("reticulum-node-core"))
            .unwrap();
        node_core["manifest_path"] = serde_json::Value::String(
            root.join("crates/lookalike-node-core/Cargo.toml")
                .display()
                .to_string(),
        );
        assert!(
            validate_radio_tx_dispatch_resolved_closure(
                &wrong_path.to_string(),
                &baseline_tree,
                &root,
            )
            .is_err()
        );

        let feature_drift_tree =
            baseline_tree.replace("embassy-sync v0.8.0\t\n", "embassy-sync v0.8.0\tstd\n");
        let feature_error = validate_radio_tx_dispatch_resolved_closure(
            &baseline_metadata.to_string(),
            &feature_drift_tree,
            &root,
        )
        .unwrap_err();
        assert!(
            feature_error.contains("embassy-sync") && feature_error.contains("features"),
            "{feature_error}"
        );
    }

    #[test]
    fn lxmf_wire_resolved_closure_is_an_exact_bare_metal_identity_and_feature_set() {
        let root = workspace_root();
        let (baseline_metadata, baseline_tree) = lxmf_wire_reviewed_closure_fixture(&root);
        validate_lxmf_wire_resolved_closure(&baseline_metadata.to_string(), &baseline_tree, &root)
            .unwrap();

        for (name, version) in [
            ("rete-core", "0.1.0"),
            ("alloc-wrapper", "1.0.0"),
            ("std-wrapper", "1.0.0"),
        ] {
            let mut metadata = baseline_metadata.clone();
            let mut tree = baseline_tree.clone();
            add_registry_closure_fixture_package(&mut metadata, &mut tree, &root, name, version);
            let error = validate_lxmf_wire_resolved_closure(&metadata.to_string(), &tree, &root)
                .unwrap_err();
            assert!(error.contains(name), "registry package {name}: {error}");
        }

        let mut local_metadata = baseline_metadata.clone();
        let mut local_tree = baseline_tree.clone();
        add_local_closure_fixture_package(
            &mut local_metadata,
            &mut local_tree,
            &root,
            "reticulum-board-heltec-vision-master-e290",
            "0.1.0",
            "crates/board-heltec-vision-master-e290/Cargo.toml",
        );
        let local_error =
            validate_lxmf_wire_resolved_closure(&local_metadata.to_string(), &local_tree, &root)
                .unwrap_err();
        assert!(local_error.contains("reticulum-board-heltec-vision-master-e290"));

        let mut wrong_version = baseline_metadata.clone();
        wrong_version["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|package| package["name"].as_str() == Some("sha2"))
            .unwrap()["version"] = serde_json::json!("0.10.8");
        let wrong_version_tree = baseline_tree.replace("sha2 v0.10.9\t", "sha2 v0.10.8\t");
        let wrong_version_error = validate_lxmf_wire_resolved_closure(
            &wrong_version.to_string(),
            &wrong_version_tree,
            &root,
        )
        .unwrap_err();
        assert!(wrong_version_error.contains("sha2"));

        let mut wrong_source = baseline_metadata.clone();
        wrong_source["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|package| package["name"].as_str() == Some("hkdf"))
            .unwrap()["source"] = serde_json::json!("registry+https://example.invalid/index");
        let wrong_source_error =
            validate_lxmf_wire_resolved_closure(&wrong_source.to_string(), &baseline_tree, &root)
                .unwrap_err();
        assert!(wrong_source_error.contains("hkdf"));

        let mut wrong_path = baseline_metadata.clone();
        wrong_path["packages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|package| package["name"].as_str() == Some("reticulum-lxmf-wire"))
            .unwrap()["manifest_path"] =
            serde_json::json!(root.join("crates/lookalike-lxmf-wire/Cargo.toml"));
        let wrong_path_error =
            validate_lxmf_wire_resolved_closure(&wrong_path.to_string(), &baseline_tree, &root)
                .unwrap_err();
        assert!(wrong_path_error.contains("reticulum-lxmf-wire"));

        for (reviewed, changed, package) in [
            (
                "ed25519-dalek v2.2.0\thazmat\n",
                "ed25519-dalek v2.2.0\talloc,hazmat\n",
                "ed25519-dalek",
            ),
            ("sha2 v0.10.9\t\n", "sha2 v0.10.9\tstd\n", "sha2"),
            (
                "ed25519-dalek v2.2.0\thazmat\n",
                "ed25519-dalek v2.2.0\thazmat,hazmat\n",
                "ed25519-dalek",
            ),
        ] {
            let changed_tree = baseline_tree.replace(reviewed, changed);
            let error = validate_lxmf_wire_resolved_closure(
                &baseline_metadata.to_string(),
                &changed_tree,
                &root,
            )
            .unwrap_err();
            assert!(
                error.contains(package),
                "feature drift for {package}: {error}"
            );
        }

        let missing_tree = baseline_tree.replace("hmac v0.12.1\t\n", "");
        let missing_error = validate_lxmf_wire_resolved_closure(
            &baseline_metadata.to_string(),
            &missing_tree,
            &root,
        )
        .unwrap_err();
        assert!(missing_error.contains("hmac"));
    }

    #[test]
    fn rns_inbox_store_boundary_allows_only_two_exact_registry_edges() {
        let root = workspace_root();
        let baseline = portable_layers_metadata_fixture(&root);
        validate_rns_inbox_store_dependency_boundary(&baseline.to_string(), &root).unwrap();

        let mut wrong_manifest = baseline.clone();
        fixture_package_mut(&mut wrong_manifest, "reticulum-rns-inbox-store")["manifest_path"] =
            serde_json::json!(root.join("crates/lookalike-inbox-store/Cargo.toml"));
        assert!(
            validate_rns_inbox_store_dependency_boundary(&wrong_manifest.to_string(), &root)
                .is_err()
        );

        let mut nonlocal = baseline.clone();
        fixture_package_mut(&mut nonlocal, "reticulum-rns-inbox-store")["source"] =
            serde_json::json!(CRATES_IO_SOURCE);
        assert!(
            validate_rns_inbox_store_dependency_boundary(&nonlocal.to_string(), &root).is_err()
        );

        let mut duplicate = baseline.clone();
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(rns_inbox_store_package_fixture(&root));
        assert!(
            validate_rns_inbox_store_dependency_boundary(&duplicate.to_string(), &root).is_err()
        );

        for features in [
            serde_json::json!({}),
            serde_json::json!({"default": ["unreviewed"]}),
            serde_json::json!({"default": [], "future": []}),
        ] {
            let mut changed = baseline.clone();
            fixture_package_mut(&mut changed, "reticulum-rns-inbox-store")["features"] = features;
            assert!(
                validate_rns_inbox_store_dependency_boundary(&changed.to_string(), &root).is_err()
            );
        }

        for dependency_name in ["embedded-storage", "sha2"] {
            for (field, value) in [
                ("name", serde_json::json!("unreviewed-replacement")),
                ("req", serde_json::json!(">=0.0.0")),
                (
                    "source",
                    serde_json::json!("registry+https://example.invalid/index"),
                ),
                ("path", serde_json::json!(root.join("crates/lookalike"))),
                ("kind", serde_json::json!("dev")),
                ("optional", serde_json::json!(true)),
                ("rename", serde_json::json!("renamed-dependency")),
                ("target", serde_json::json!("cfg(target_os = \"none\")")),
                ("uses_default_features", serde_json::json!(true)),
                ("features", serde_json::json!(["unreviewed"])),
            ] {
                let mut changed = baseline.clone();
                fixture_dependency_mut(
                    fixture_package_mut(&mut changed, "reticulum-rns-inbox-store"),
                    dependency_name,
                    None,
                )[field] = value;
                assert!(
                    validate_rns_inbox_store_dependency_boundary(&changed.to_string(), &root)
                        .is_err(),
                    "{dependency_name} accepted changed {field}"
                );
            }
        }

        let mut build = baseline.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut build, "reticulum-rns-inbox-store"),
            "sha2",
            None,
        )["kind"] = serde_json::json!("build");
        assert!(validate_rns_inbox_store_dependency_boundary(&build.to_string(), &root).is_err());

        let mut extra = baseline;
        fixture_package_mut(&mut extra, "reticulum-rns-inbox-store")["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("heapless", "=0.9.1", None));
        assert!(validate_rns_inbox_store_dependency_boundary(&extra.to_string(), &root).is_err());
    }

    #[test]
    fn storage_model_boundary_rejects_unreviewed_dependencies_features_and_edge_shapes() {
        let root = workspace_root();
        const PACKAGE_INDEX: usize = 5;

        let mut wrong_manifest = portable_layers_metadata_fixture(&root);
        wrong_manifest["packages"][PACKAGE_INDEX]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_storage_model_dependency_boundary(&wrong_manifest.to_string(), &root).is_err()
        );

        let mut duplicate = portable_layers_metadata_fixture(&root);
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(storage_model_package_fixture(&root));
        assert!(validate_storage_model_dependency_boundary(&duplicate.to_string(), &root).is_err());

        for (dependency_index, wrong_requirement) in [(0, "=2.2.1"), (1, "=0.10.8")] {
            let mut wrong_version = portable_layers_metadata_fixture(&root);
            wrong_version["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["req"] =
                serde_json::Value::String(wrong_requirement.to_owned());
            assert!(
                validate_storage_model_dependency_boundary(&wrong_version.to_string(), &root)
                    .is_err(),
                "dependency {dependency_index} accepted {wrong_requirement}"
            );

            let mut default_features = portable_layers_metadata_fixture(&root);
            default_features["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["uses_default_features"] =
                serde_json::Value::Bool(true);
            assert!(
                validate_storage_model_dependency_boundary(&default_features.to_string(), &root)
                    .is_err(),
                "dependency {dependency_index} accepted default features"
            );

            let mut dependency_feature = portable_layers_metadata_fixture(&root);
            dependency_feature["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["features"] =
                serde_json::json!(["unreviewed"]);
            assert!(
                validate_storage_model_dependency_boundary(&dependency_feature.to_string(), &root)
                    .is_err(),
                "dependency {dependency_index} accepted an unreviewed feature"
            );

            for (field, value) in [
                ("optional", serde_json::Value::Bool(true)),
                (
                    "rename",
                    serde_json::Value::String("renamed-dependency".to_owned()),
                ),
                (
                    "target",
                    serde_json::Value::String("cfg(target_os = \"none\")".to_owned()),
                ),
                (
                    "path",
                    serde_json::Value::String(root.join("elsewhere").display().to_string()),
                ),
            ] {
                let mut changed = portable_layers_metadata_fixture(&root);
                changed["packages"][PACKAGE_INDEX]["dependencies"][dependency_index][field] = value;
                assert!(
                    validate_storage_model_dependency_boundary(&changed.to_string(), &root)
                        .is_err(),
                    "dependency {dependency_index} accepted changed {field}"
                );
            }
        }

        let mut development = portable_layers_metadata_fixture(&root);
        development["packages"][PACKAGE_INDEX]["dependencies"][0]["kind"] =
            serde_json::Value::String("dev".to_owned());
        assert!(
            validate_storage_model_dependency_boundary(&development.to_string(), &root).is_err()
        );

        let mut extra = portable_layers_metadata_fixture(&root);
        extra["packages"][PACKAGE_INDEX]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("rand_core", "=0.6.4", None));
        assert!(validate_storage_model_dependency_boundary(&extra.to_string(), &root).is_err());

        let mut extra_feature = portable_layers_metadata_fixture(&root);
        extra_feature["packages"][PACKAGE_INDEX]["features"]["std"] = serde_json::json!([]);
        assert!(
            validate_storage_model_dependency_boundary(&extra_feature.to_string(), &root).is_err()
        );
    }

    #[test]
    fn storage_journal_boundary_rejects_unreviewed_dependencies_features_and_edge_shapes() {
        const PACKAGE_INDEX: usize = 7;
        let root = workspace_root();

        let mut extra = portable_layers_metadata_fixture(&root);
        extra["packages"][PACKAGE_INDEX]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("heapless", "=0.9.1", None));
        assert!(validate_storage_journal_dependency_boundary(&extra.to_string(), &root).is_err());

        let mut default_features = portable_layers_metadata_fixture(&root);
        default_features["packages"][PACKAGE_INDEX]["dependencies"][0]["uses_default_features"] =
            serde_json::Value::Bool(true);
        assert!(
            validate_storage_journal_dependency_boundary(&default_features.to_string(), &root)
                .is_err()
        );

        let mut wrong_path = portable_layers_metadata_fixture(&root);
        wrong_path["packages"][PACKAGE_INDEX]["dependencies"][1]["path"] =
            serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_storage_journal_dependency_boundary(&wrong_path.to_string(), &root).is_err()
        );

        let mut build = portable_layers_metadata_fixture(&root);
        build["packages"][PACKAGE_INDEX]["dependencies"][2]["kind"] =
            serde_json::Value::String("build".to_owned());
        assert!(validate_storage_journal_dependency_boundary(&build.to_string(), &root).is_err());

        let mut feature = portable_layers_metadata_fixture(&root);
        feature["packages"][PACKAGE_INDEX]["features"]["alloc"] = serde_json::json!([]);
        assert!(validate_storage_journal_dependency_boundary(&feature.to_string(), &root).is_err());
    }

    #[test]
    fn storage_actor_boundary_rejects_dependency_feature_and_path_drift() {
        let root = workspace_root();

        let mut wrong_manifest = portable_layers_metadata_fixture(&root);
        fixture_package_mut(&mut wrong_manifest, "reticulum-storage-actor")["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_storage_actor_dependency_boundary(&wrong_manifest.to_string(), &root).is_err()
        );

        let mut duplicate = portable_layers_metadata_fixture(&root);
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(storage_actor_package_fixture(&root));
        assert!(validate_storage_actor_dependency_boundary(&duplicate.to_string(), &root).is_err());

        let dependencies = [
            ("embedded-storage", None),
            ("reticulum-node-core", None),
            ("reticulum-storage-journal", None),
            ("reticulum-storage-model", None),
            ("reticulum-submission-projector", None),
            ("rand_core", Some("dev")),
        ];
        for (name, kind) in dependencies {
            let mut default_features = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut default_features, "reticulum-storage-actor"),
                name,
                kind,
            )["uses_default_features"] = serde_json::Value::Bool(true);
            assert!(
                validate_storage_actor_dependency_boundary(&default_features.to_string(), &root)
                    .is_err(),
                "dependency {name} accepted default features"
            );

            let mut dependency_feature = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut dependency_feature, "reticulum-storage-actor"),
                name,
                kind,
            )["features"] = serde_json::json!(["unreviewed"]);
            assert!(
                validate_storage_actor_dependency_boundary(&dependency_feature.to_string(), &root)
                    .is_err(),
                "dependency {name} accepted an unreviewed feature"
            );

            for (field, value) in [
                ("optional", serde_json::Value::Bool(true)),
                (
                    "rename",
                    serde_json::Value::String("renamed-dependency".to_owned()),
                ),
                (
                    "target",
                    serde_json::Value::String("cfg(target_os = \"none\")".to_owned()),
                ),
            ] {
                let mut changed = portable_layers_metadata_fixture(&root);
                fixture_dependency_mut(
                    fixture_package_mut(&mut changed, "reticulum-storage-actor"),
                    name,
                    kind,
                )[field] = value;
                assert!(
                    validate_storage_actor_dependency_boundary(&changed.to_string(), &root)
                        .is_err(),
                    "dependency {name} accepted changed {field}"
                );
            }

            let mut wrong_requirement = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut wrong_requirement, "reticulum-storage-actor"),
                name,
                kind,
            )["req"] = serde_json::Value::String("=99.0.0".to_owned());
            assert!(
                validate_storage_actor_dependency_boundary(&wrong_requirement.to_string(), &root)
                    .is_err(),
                "dependency {name} accepted the wrong requirement"
            );

            let mut wrong_kind = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut wrong_kind, "reticulum-storage-actor"),
                name,
                kind,
            )["kind"] = match kind {
                None => serde_json::Value::String("dev".to_owned()),
                Some("dev") => serde_json::Value::Null,
                Some(other) => panic!("unexpected fixture dependency kind {other}"),
            };
            assert!(
                validate_storage_actor_dependency_boundary(&wrong_kind.to_string(), &root).is_err(),
                "dependency {name} accepted the wrong kind"
            );
        }

        for (name, kind) in [
            ("reticulum-node-core", None),
            ("reticulum-storage-journal", None),
            ("reticulum-storage-model", None),
            ("reticulum-submission-projector", None),
        ] {
            let mut wrong_path = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut wrong_path, "reticulum-storage-actor"),
                name,
                kind,
            )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_storage_actor_dependency_boundary(&wrong_path.to_string(), &root).is_err(),
                "local dependency {name} accepted the wrong path"
            );
        }

        for (name, kind) in [("embedded-storage", None), ("rand_core", Some("dev"))] {
            let mut registry_path = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut registry_path, "reticulum-storage-actor"),
                name,
                kind,
            )["path"] = serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_storage_actor_dependency_boundary(&registry_path.to_string(), &root)
                    .is_err(),
                "registry dependency {name} accepted a path"
            );
        }

        let mut build = portable_layers_metadata_fixture(&root);
        fixture_dependency_mut(
            fixture_package_mut(&mut build, "reticulum-storage-actor"),
            "embedded-storage",
            None,
        )["kind"] = serde_json::Value::String("build".to_owned());
        assert!(validate_storage_actor_dependency_boundary(&build.to_string(), &root).is_err());

        let mut extra = portable_layers_metadata_fixture(&root);
        fixture_package_mut(&mut extra, "reticulum-storage-actor")["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("sha2", "=0.10.9", None));
        assert!(validate_storage_actor_dependency_boundary(&extra.to_string(), &root).is_err());

        let mut extra_feature = portable_layers_metadata_fixture(&root);
        fixture_package_mut(&mut extra_feature, "reticulum-storage-actor")["features"]["std"] =
            serde_json::json!([]);
        assert!(
            validate_storage_actor_dependency_boundary(&extra_feature.to_string(), &root).is_err()
        );
    }

    #[test]
    fn submission_runtime_boundary_rejects_platform_radio_and_edge_drift() {
        let root = workspace_root();
        let baseline = portable_layers_metadata_fixture(&root);
        validate_submission_runtime_dependency_boundary(&baseline.to_string(), &root).unwrap();

        let mut wrong_manifest = baseline.clone();
        fixture_package_mut(&mut wrong_manifest, "reticulum-submission-runtime")["manifest_path"] =
            serde_json::Value::String(
                root.join("crates/lookalike-runtime/Cargo.toml")
                    .display()
                    .to_string(),
            );
        assert!(
            validate_submission_runtime_dependency_boundary(&wrong_manifest.to_string(), &root)
                .is_err()
        );

        let mut wrong_node_path = baseline.clone();
        fixture_dependency_mut(
            fixture_package_mut(&mut wrong_node_path, "reticulum-submission-runtime"),
            "reticulum-node-core",
            None,
        )["path"] = serde_json::Value::String(
            root.join("crates/lookalike-node-core")
                .display()
                .to_string(),
        );
        assert!(
            validate_submission_runtime_dependency_boundary(&wrong_node_path.to_string(), &root)
                .is_err()
        );

        let mut radio_edge = baseline.clone();
        fixture_package_mut(&mut radio_edge, "reticulum-submission-runtime")["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_path_dependency_fixture(
                "reticulum-radio-tx-dispatch",
                "*",
                &root.join("crates/radio-tx-dispatch"),
                None,
            ));
        assert!(
            validate_submission_runtime_dependency_boundary(&radio_edge.to_string(), &root)
                .is_err()
        );

        let mut feature = baseline;
        fixture_package_mut(&mut feature, "reticulum-submission-runtime")["features"]["lora"] =
            serde_json::json!([]);
        assert!(
            validate_submission_runtime_dependency_boundary(&feature.to_string(), &root).is_err()
        );
    }

    #[test]
    fn device_api_adapter_boundary_rejects_dependency_feature_and_path_drift() {
        const PACKAGE_INDEX: usize = 9;
        let root = workspace_root();

        let mut wrong_manifest = portable_layers_metadata_fixture(&root);
        wrong_manifest["packages"][PACKAGE_INDEX]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_device_api_adapter_dependency_boundary(&wrong_manifest.to_string(), &root)
                .is_err()
        );

        let mut duplicate = portable_layers_metadata_fixture(&root);
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(device_api_adapter_package_fixture(&root));
        assert!(
            validate_device_api_adapter_dependency_boundary(&duplicate.to_string(), &root).is_err()
        );

        for (feature, value) in [
            ("default", serde_json::json!(["experimental-rns-data"])),
            ("experimental-rns-data", serde_json::json!([])),
            (
                "experimental-rns-data",
                serde_json::json!(["reticulum-device-api/unreviewed"]),
            ),
            ("experimental-rns-inbox", serde_json::json!([])),
            (
                "experimental-rns-inbox",
                serde_json::json!(["reticulum-device-api/unreviewed"]),
            ),
        ] {
            let mut feature_drift = portable_layers_metadata_fixture(&root);
            feature_drift["packages"][PACKAGE_INDEX]["features"][feature] = value;
            assert!(
                validate_device_api_adapter_dependency_boundary(&feature_drift.to_string(), &root)
                    .is_err(),
                "adapter accepted drift in {feature}"
            );
        }

        let mut extra_feature = portable_layers_metadata_fixture(&root);
        extra_feature["packages"][PACKAGE_INDEX]["features"]["std"] = serde_json::json!([]);
        assert!(
            validate_device_api_adapter_dependency_boundary(&extra_feature.to_string(), &root)
                .is_err()
        );

        let mut wrong_version = portable_layers_metadata_fixture(&root);
        wrong_version["packages"][PACKAGE_INDEX]["dependencies"][0]["req"] =
            serde_json::Value::String("=0.3.0".to_owned());
        assert!(
            validate_device_api_adapter_dependency_boundary(&wrong_version.to_string(), &root)
                .is_err()
        );

        for dependency_index in 0..=3 {
            let mut default_features = portable_layers_metadata_fixture(&root);
            default_features["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["uses_default_features"] =
                serde_json::Value::Bool(true);
            assert!(
                validate_device_api_adapter_dependency_boundary(
                    &default_features.to_string(),
                    &root
                )
                .is_err(),
                "dependency {dependency_index} accepted default features"
            );

            let mut dependency_feature = portable_layers_metadata_fixture(&root);
            dependency_feature["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["features"] =
                serde_json::json!(["unreviewed"]);
            assert!(
                validate_device_api_adapter_dependency_boundary(
                    &dependency_feature.to_string(),
                    &root
                )
                .is_err(),
                "dependency {dependency_index} accepted an unreviewed feature"
            );

            for (field, value) in [
                ("optional", serde_json::Value::Bool(true)),
                (
                    "rename",
                    serde_json::Value::String("renamed-dependency".to_owned()),
                ),
                (
                    "target",
                    serde_json::Value::String("cfg(target_os = \"none\")".to_owned()),
                ),
            ] {
                let mut changed = portable_layers_metadata_fixture(&root);
                changed["packages"][PACKAGE_INDEX]["dependencies"][dependency_index][field] = value;
                assert!(
                    validate_device_api_adapter_dependency_boundary(&changed.to_string(), &root)
                        .is_err(),
                    "dependency {dependency_index} accepted changed {field}"
                );
            }
        }

        for dependency_index in 1..=3 {
            let mut wrong_path = portable_layers_metadata_fixture(&root);
            wrong_path["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["path"] =
                serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_device_api_adapter_dependency_boundary(&wrong_path.to_string(), &root)
                    .is_err(),
                "local dependency {dependency_index} accepted the wrong path"
            );

            let mut wrong_requirement = portable_layers_metadata_fixture(&root);
            wrong_requirement["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["req"] =
                serde_json::Value::String("=0.1.0".to_owned());
            assert!(
                validate_device_api_adapter_dependency_boundary(
                    &wrong_requirement.to_string(),
                    &root
                )
                .is_err(),
                "local dependency {dependency_index} accepted a registry-like requirement"
            );
        }

        let mut registry_path = portable_layers_metadata_fixture(&root);
        registry_path["packages"][PACKAGE_INDEX]["dependencies"][0]["path"] =
            serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_device_api_adapter_dependency_boundary(&registry_path.to_string(), &root)
                .is_err()
        );

        let mut registry_source = portable_layers_metadata_fixture(&root);
        registry_source["packages"][PACKAGE_INDEX]["dependencies"][0]["source"] =
            serde_json::Value::Null;
        assert!(
            validate_device_api_adapter_dependency_boundary(&registry_source.to_string(), &root)
                .is_err()
        );

        let mut local_source = portable_layers_metadata_fixture(&root);
        local_source["packages"][PACKAGE_INDEX]["dependencies"][1]["source"] =
            serde_json::Value::String(
                "registry+https://github.com/rust-lang/crates.io-index".to_owned(),
            );
        assert!(
            validate_device_api_adapter_dependency_boundary(&local_source.to_string(), &root)
                .is_err()
        );

        for (dependency_index, kind) in [(0, None), (1, Some("dev")), (2, None), (3, Some("dev"))] {
            let mut wrong_kind = portable_layers_metadata_fixture(&root);
            wrong_kind["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["kind"] = kind
                .map_or(serde_json::Value::Null, |kind| {
                    serde_json::Value::String(kind.to_owned())
                });
            assert!(
                validate_device_api_adapter_dependency_boundary(&wrong_kind.to_string(), &root)
                    .is_err(),
                "adapter accepted the wrong kind for dependency {dependency_index}"
            );
        }

        let mut build_dependency = portable_layers_metadata_fixture(&root);
        build_dependency["packages"][PACKAGE_INDEX]["dependencies"][1]["kind"] =
            serde_json::Value::String("build".to_owned());
        assert!(
            validate_device_api_adapter_dependency_boundary(&build_dependency.to_string(), &root)
                .is_err()
        );

        let mut extra = portable_layers_metadata_fixture(&root);
        extra["packages"][PACKAGE_INDEX]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("sha2", "=0.10.9", None));
        assert!(
            validate_device_api_adapter_dependency_boundary(&extra.to_string(), &root).is_err()
        );
    }

    #[test]
    fn submission_projector_boundary_rejects_dependency_feature_and_edge_drift() {
        let root = workspace_root();
        const PACKAGE_INDEX: usize = 6;

        let mut wrong_manifest = portable_layers_metadata_fixture(&root);
        wrong_manifest["packages"][PACKAGE_INDEX]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_submission_projector_dependency_boundary(&wrong_manifest.to_string(), &root)
                .is_err()
        );

        let mut duplicate = portable_layers_metadata_fixture(&root);
        duplicate["packages"]
            .as_array_mut()
            .unwrap()
            .push(submission_projector_package_fixture(&root));
        assert!(
            validate_submission_projector_dependency_boundary(&duplicate.to_string(), &root)
                .is_err()
        );

        for dependency_index in 0..=1 {
            let mut wrong_path = portable_layers_metadata_fixture(&root);
            wrong_path["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["path"] =
                serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_submission_projector_dependency_boundary(&wrong_path.to_string(), &root)
                    .is_err(),
                "local dependency {dependency_index} accepted the wrong path"
            );

            let mut wrong_requirement = portable_layers_metadata_fixture(&root);
            wrong_requirement["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["req"] =
                serde_json::Value::String("=0.1.0".to_owned());
            assert!(
                validate_submission_projector_dependency_boundary(
                    &wrong_requirement.to_string(),
                    &root
                )
                .is_err(),
                "local dependency {dependency_index} accepted a registry-like requirement"
            );

            let mut default_features = portable_layers_metadata_fixture(&root);
            default_features["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["uses_default_features"] =
                serde_json::Value::Bool(true);
            assert!(
                validate_submission_projector_dependency_boundary(
                    &default_features.to_string(),
                    &root
                )
                .is_err(),
                "local dependency {dependency_index} accepted default features"
            );

            let mut dependency_feature = portable_layers_metadata_fixture(&root);
            dependency_feature["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["features"] =
                serde_json::json!(["unreviewed"]);
            assert!(
                validate_submission_projector_dependency_boundary(
                    &dependency_feature.to_string(),
                    &root
                )
                .is_err(),
                "local dependency {dependency_index} accepted an unreviewed feature"
            );

            for (field, value) in [
                ("optional", serde_json::Value::Bool(true)),
                (
                    "rename",
                    serde_json::Value::String("renamed-dependency".to_owned()),
                ),
                (
                    "target",
                    serde_json::Value::String("cfg(target_os = \"none\")".to_owned()),
                ),
            ] {
                let mut changed = portable_layers_metadata_fixture(&root);
                changed["packages"][PACKAGE_INDEX]["dependencies"][dependency_index][field] = value;
                assert!(
                    validate_submission_projector_dependency_boundary(&changed.to_string(), &root)
                        .is_err(),
                    "local dependency {dependency_index} accepted changed {field}"
                );
            }
        }

        let mut wrong_dev_version = portable_layers_metadata_fixture(&root);
        wrong_dev_version["packages"][PACKAGE_INDEX]["dependencies"][2]["req"] =
            serde_json::Value::String("=0.6.3".to_owned());
        assert!(
            validate_submission_projector_dependency_boundary(
                &wrong_dev_version.to_string(),
                &root
            )
            .is_err()
        );

        let mut dev_defaults = portable_layers_metadata_fixture(&root);
        dev_defaults["packages"][PACKAGE_INDEX]["dependencies"][2]["uses_default_features"] =
            serde_json::Value::Bool(true);
        assert!(
            validate_submission_projector_dependency_boundary(&dev_defaults.to_string(), &root)
                .is_err()
        );

        let mut dev_feature = portable_layers_metadata_fixture(&root);
        dev_feature["packages"][PACKAGE_INDEX]["dependencies"][2]["features"] =
            serde_json::json!(["unreviewed"]);
        assert!(
            validate_submission_projector_dependency_boundary(&dev_feature.to_string(), &root)
                .is_err()
        );

        let mut build = portable_layers_metadata_fixture(&root);
        build["packages"][PACKAGE_INDEX]["dependencies"][2]["kind"] =
            serde_json::Value::String("build".to_owned());
        assert!(
            validate_submission_projector_dependency_boundary(&build.to_string(), &root).is_err()
        );

        let mut extra = portable_layers_metadata_fixture(&root);
        extra["packages"][PACKAGE_INDEX]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture("sha2", "=0.10.9", None));
        assert!(
            validate_submission_projector_dependency_boundary(&extra.to_string(), &root).is_err()
        );

        let mut extra_feature = portable_layers_metadata_fixture(&root);
        extra_feature["packages"][PACKAGE_INDEX]["features"]["std"] = serde_json::json!([]);
        assert!(
            validate_submission_projector_dependency_boundary(&extra_feature.to_string(), &root)
                .is_err()
        );
    }

    #[test]
    fn tx_supervisor_boundary_rejects_unreviewed_dependencies_features_and_edge_shapes() {
        let root = workspace_root();
        const PACKAGE_INDEX: usize = 4;

        let mut wrong_manifest = portable_layers_metadata_fixture(&root);
        wrong_manifest["packages"][PACKAGE_INDEX]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_tx_supervisor_dependency_boundary(&wrong_manifest.to_string(), &root).is_err()
        );

        for dependency_index in [0, 1, 6, 7] {
            let mut wrong_path = portable_layers_metadata_fixture(&root);
            wrong_path["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["path"] =
                serde_json::Value::String(root.join("elsewhere").display().to_string());
            assert!(
                validate_tx_supervisor_dependency_boundary(&wrong_path.to_string(), &root).is_err(),
                "local normal dependency {dependency_index} accepted the wrong path"
            );

            let mut default_features = portable_layers_metadata_fixture(&root);
            default_features["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["uses_default_features"] =
                serde_json::Value::Bool(true);
            assert!(
                validate_tx_supervisor_dependency_boundary(&default_features.to_string(), &root)
                    .is_err(),
                "local normal dependency {dependency_index} accepted default features"
            );
        }

        for (dependency_index, wrong_requirement) in
            [(2, "=0.7.2"), (3, "=0.1.1"), (4, "=0.4.0"), (5, "=0.6.3")]
        {
            let mut wrong_version = portable_layers_metadata_fixture(&root);
            wrong_version["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["req"] =
                serde_json::Value::String(wrong_requirement.to_owned());
            assert!(
                validate_tx_supervisor_dependency_boundary(&wrong_version.to_string(), &root)
                    .is_err(),
                "registry normal dependency {dependency_index} accepted {wrong_requirement}"
            );

            let mut default_features = portable_layers_metadata_fixture(&root);
            default_features["packages"][PACKAGE_INDEX]["dependencies"][dependency_index]["uses_default_features"] =
                serde_json::Value::Bool(true);
            assert!(
                validate_tx_supervisor_dependency_boundary(&default_features.to_string(), &root)
                    .is_err(),
                "registry normal dependency {dependency_index} accepted default features"
            );
        }

        let mut wrong_handoff_path = portable_layers_metadata_fixture(&root);
        wrong_handoff_path["packages"][PACKAGE_INDEX]["dependencies"][7]["path"] =
            serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_tx_supervisor_dependency_boundary(&wrong_handoff_path.to_string(), &root)
                .is_err()
        );

        let mut handoff_defaults = portable_layers_metadata_fixture(&root);
        handoff_defaults["packages"][PACKAGE_INDEX]["dependencies"][7]["uses_default_features"] =
            serde_json::Value::Bool(true);
        assert!(
            validate_tx_supervisor_dependency_boundary(&handoff_defaults.to_string(), &root)
                .is_err()
        );

        let mut static_cell_without_defaults = portable_layers_metadata_fixture(&root);
        static_cell_without_defaults["packages"][PACKAGE_INDEX]["dependencies"][9]["uses_default_features"] =
            serde_json::Value::Bool(false);
        assert!(
            validate_tx_supervisor_dependency_boundary(
                &static_cell_without_defaults.to_string(),
                &root
            )
            .is_err()
        );

        let mut wrong_rns_path = portable_layers_metadata_fixture(&root);
        wrong_rns_path["packages"][PACKAGE_INDEX]["dependencies"][8]["path"] =
            serde_json::Value::String(root.join("elsewhere").display().to_string());
        assert!(
            validate_tx_supervisor_dependency_boundary(&wrong_rns_path.to_string(), &root).is_err()
        );

        let mut rns_defaults = portable_layers_metadata_fixture(&root);
        rns_defaults["packages"][PACKAGE_INDEX]["dependencies"][8]["uses_default_features"] =
            serde_json::Value::Bool(true);
        assert!(
            validate_tx_supervisor_dependency_boundary(&rns_defaults.to_string(), &root).is_err()
        );

        let mut extra = portable_layers_metadata_fixture(&root);
        extra["packages"][PACKAGE_INDEX]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(handoff_dependency_fixture(
                "embassy-executor",
                "=0.10.0",
                None,
            ));
        assert!(validate_tx_supervisor_dependency_boundary(&extra.to_string(), &root).is_err());

        let mut build = portable_layers_metadata_fixture(&root);
        let mut build_dependency = handoff_dependency_fixture("cc", "=1.0.0", Some("dev"));
        build_dependency["kind"] = serde_json::Value::String("build".to_owned());
        build["packages"][PACKAGE_INDEX]["dependencies"]
            .as_array_mut()
            .unwrap()
            .push(build_dependency);
        assert!(validate_tx_supervisor_dependency_boundary(&build.to_string(), &root).is_err());

        let mut extra_feature = portable_layers_metadata_fixture(&root);
        extra_feature["packages"][PACKAGE_INDEX]["features"]["rf"] = serde_json::json!([]);
        assert!(
            validate_tx_supervisor_dependency_boundary(&extra_feature.to_string(), &root).is_err()
        );
    }

    #[test]
    fn portable_durability_boundaries_accept_only_exact_local_packages_and_features() {
        let root = workspace_root();
        let baseline = portable_layers_metadata_fixture(&root);
        validate_portable_durability_dependency_boundaries(&baseline.to_string(), &root).unwrap();

        for package_name in [
            "reticulum-device-identity-store",
            "reticulum-device-api-credential-store",
            "reticulum-announce-clock",
            "reticulum-nor-flash-region",
        ] {
            let mut wrong_manifest = portable_layers_metadata_fixture(&root);
            fixture_package_mut(&mut wrong_manifest, package_name)["manifest_path"] =
                serde_json::Value::String(
                    root.join("crates/lookalike/Cargo.toml")
                        .display()
                        .to_string(),
                );
            assert!(
                validate_portable_durability_dependency_boundaries(
                    &wrong_manifest.to_string(),
                    &root,
                )
                .is_err(),
                "{package_name} accepted a lookalike manifest path"
            );

            let mut nonlocal = portable_layers_metadata_fixture(&root);
            fixture_package_mut(&mut nonlocal, package_name)["source"] =
                serde_json::Value::String(CRATES_IO_SOURCE.to_owned());
            assert!(
                validate_portable_durability_dependency_boundaries(&nonlocal.to_string(), &root,)
                    .is_err(),
                "{package_name} accepted a nonlocal package"
            );

            let mut duplicate = portable_layers_metadata_fixture(&root);
            let duplicate_package = fixture_package_mut(&mut duplicate, package_name).clone();
            duplicate["packages"]
                .as_array_mut()
                .unwrap()
                .push(duplicate_package);
            assert!(
                validate_portable_durability_dependency_boundaries(&duplicate.to_string(), &root,)
                    .is_err(),
                "{package_name} accepted a duplicate package"
            );

            let mut extra_feature = portable_layers_metadata_fixture(&root);
            fixture_package_mut(&mut extra_feature, package_name)["features"]["future"] =
                serde_json::json!([]);
            assert!(
                validate_portable_durability_dependency_boundaries(
                    &extra_feature.to_string(),
                    &root,
                )
                .is_err(),
                "{package_name} accepted an unreviewed feature"
            );
        }
    }

    #[test]
    fn portable_durability_boundaries_reject_all_dependency_shape_drift() {
        let root = workspace_root();
        for (package_name, dependency_name) in [
            ("reticulum-device-identity-store", "embedded-storage"),
            ("reticulum-device-identity-store", "rand_core"),
            ("reticulum-device-identity-store", "sha2"),
            ("reticulum-device-identity-store", "zeroize"),
            ("reticulum-device-api-credential-store", "embedded-storage"),
            ("reticulum-device-api-credential-store", "sha2"),
            ("reticulum-device-api-credential-store", "zeroize"),
            ("reticulum-announce-clock", "embedded-storage"),
            ("reticulum-announce-clock", "sha2"),
            ("reticulum-nor-flash-region", "embedded-storage"),
        ] {
            for (field, value) in [
                ("name", serde_json::json!("unreviewed-replacement")),
                ("req", serde_json::json!(">=0.0.0")),
                (
                    "source",
                    serde_json::json!("registry+https://example.invalid/index"),
                ),
                ("path", serde_json::json!(root.join("crates/lookalike"))),
                ("kind", serde_json::json!("dev")),
                ("optional", serde_json::json!(true)),
                ("rename", serde_json::json!("renamed-dependency")),
                ("target", serde_json::json!("cfg(target_os = \"none\")")),
                ("uses_default_features", serde_json::json!(true)),
                ("features", serde_json::json!(["unreviewed"])),
            ] {
                let mut changed = portable_layers_metadata_fixture(&root);
                fixture_dependency_mut(
                    fixture_package_mut(&mut changed, package_name),
                    dependency_name,
                    None,
                )[field] = value;
                assert!(
                    validate_portable_durability_dependency_boundaries(
                        &changed.to_string(),
                        &root,
                    )
                    .is_err(),
                    "{package_name} {dependency_name} accepted changed {field}"
                );
            }
        }

        for (field, value) in [
            ("name", serde_json::json!("unreviewed-replacement")),
            ("req", serde_json::json!(">=0.0.0")),
            ("source", serde_json::json!(CRATES_IO_SOURCE)),
            ("path", serde_json::json!(root.join("crates/lookalike"))),
            ("kind", serde_json::json!("dev")),
            ("optional", serde_json::json!(true)),
            ("rename", serde_json::json!("renamed-dependency")),
            ("target", serde_json::json!("cfg(target_os = \"none\")")),
            ("uses_default_features", serde_json::json!(true)),
            ("features", serde_json::json!(["unreviewed"])),
        ] {
            let mut changed = portable_layers_metadata_fixture(&root);
            fixture_dependency_mut(
                fixture_package_mut(&mut changed, "reticulum-device-api-credential-store"),
                "reticulum-device-api-credentials",
                None,
            )[field] = value;
            assert!(
                validate_portable_durability_dependency_boundaries(&changed.to_string(), &root,)
                    .is_err(),
                "credential store accepted changed local dependency {field}"
            );
        }
    }

    #[test]
    fn portable_durability_boundaries_reject_platform_radio_rete_and_async_edges() {
        let root = workspace_root();
        for package_name in [
            "reticulum-device-identity-store",
            "reticulum-device-api-credential-store",
            "reticulum-announce-clock",
            "reticulum-nor-flash-region",
        ] {
            for prohibited in [
                "esp-hal",
                "esp-storage",
                "reticulum-radio-interface",
                "lora-phy",
                "rete-core",
                "reticulum-rns-rete",
                "embassy-sync",
                "embassy-time",
            ] {
                let mut metadata = portable_layers_metadata_fixture(&root);
                fixture_package_mut(&mut metadata, package_name)["dependencies"]
                    .as_array_mut()
                    .unwrap()
                    .push(handoff_dependency_fixture(prohibited, "=0.0.0", None));
                assert!(
                    validate_portable_durability_dependency_boundaries(
                        &metadata.to_string(),
                        &root,
                    )
                    .is_err(),
                    "{package_name} accepted prohibited dependency {prohibited}"
                );
            }
        }
    }

    #[test]
    fn portable_layer_boundary_rejects_dependencies_between_the_layers() {
        let root = workspace_root();
        for (package_index, peer) in [(0, "reticulum-node-core"), (1, "reticulum-device-api")] {
            let mut metadata = portable_layers_metadata_fixture(&root);
            metadata["packages"][package_index]["dependencies"]
                .as_array_mut()
                .unwrap()
                .push(portable_dependency_fixture(peer));

            let error = validate_portable_layer_dependency_boundary(&metadata.to_string(), &root)
                .unwrap_err();
            assert!(error.contains("peer portable layer"), "{error}");
        }
    }

    #[test]
    fn portable_layer_boundary_rejects_platform_implementation_dependencies() {
        let root = workspace_root();
        for package_index in 0..=1 {
            for prohibited in [
                "reticulum-heltec-tracker-v2",
                "reticulum-board-example",
                "reticulum-radio-interface",
                "radio-sx1262",
                "lora-phy",
                "sx1262-driver",
                "esp-hal",
                "esp32-nimble",
                "embassy-sync",
            ] {
                let mut metadata = portable_layers_metadata_fixture(&root);
                metadata["packages"][package_index]["dependencies"]
                    .as_array_mut()
                    .unwrap()
                    .push(portable_dependency_fixture(prohibited));

                let error =
                    validate_portable_layer_dependency_boundary(&metadata.to_string(), &root)
                        .unwrap_err();
                assert!(
                    error.contains("platform implementation crate"),
                    "{prohibited}: {error}"
                );
            }
        }

        for relative_path in [
            "firmware/example",
            "crates/board-example",
            "crates/radio-example",
        ] {
            let mut metadata = portable_layers_metadata_fixture(&root);
            let mut dependency = portable_dependency_fixture("renamed-platform-layer");
            dependency["path"] =
                serde_json::Value::String(root.join(relative_path).display().to_string());
            metadata["packages"][0]["dependencies"]
                .as_array_mut()
                .unwrap()
                .push(dependency);
            assert!(
                validate_portable_layer_dependency_boundary(&metadata.to_string(), &root).is_err(),
                "local platform path {relative_path} was accepted"
            );
        }
    }

    #[test]
    fn portable_layer_boundary_rejects_rete_dependencies_from_device_api() {
        let root = workspace_root();
        for prohibited in ["reticulum-rns-rete", "reticulum-rns-rete-rx", "rete-core"] {
            let mut metadata = portable_layers_metadata_fixture(&root);
            metadata["packages"][0]["dependencies"]
                .as_array_mut()
                .unwrap()
                .push(portable_dependency_fixture(prohibited));

            let error = validate_portable_layer_dependency_boundary(&metadata.to_string(), &root)
                .unwrap_err();
            assert!(
                error.contains("Rete implementation crate"),
                "{prohibited}: {error}"
            );
        }
    }

    #[test]
    fn portable_layer_boundary_rejects_tx_ownership_from_device_api() {
        let root = workspace_root();
        for prohibited in [
            "reticulum-radio-tx-dispatch",
            "reticulum-tx-handoff",
            "reticulum-tx-dispatch",
            "reticulum-tx-supervisor",
        ] {
            let mut metadata = portable_layers_metadata_fixture(&root);
            metadata["packages"][0]["dependencies"]
                .as_array_mut()
                .unwrap()
                .push(portable_dependency_fixture(prohibited));

            let error = validate_portable_layer_dependency_boundary(&metadata.to_string(), &root)
                .unwrap_err();
            assert!(
                error.contains("TX ownership crate"),
                "{prohibited}: {error}"
            );
        }
    }

    fn portable_layers_metadata_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "packages": [
                {
                    "name": "reticulum-device-api",
                    "source": null,
                    "manifest_path": root.join("crates/device-api/Cargo.toml"),
                    "dependencies": [portable_dependency_fixture("minicbor")],
                },
                {
                    "name": "reticulum-node-core",
                    "source": null,
                    "manifest_path": root.join("crates/node-core/Cargo.toml"),
                    "dependencies": [
                        handoff_dependency_fixture("rand_core", "=0.6.4", None),
                        handoff_path_dependency_fixture(
                            "reticulum-rns-rete",
                            "*",
                            &root.join("crates/rns-rete"),
                            None,
                        ),
                        handoff_dependency_fixture("sha2", "=0.10.9", None),
                    ],
                },
                {
                    "name": "reticulum-tx-handoff",
                    "source": null,
                    "manifest_path": root.join("crates/tx-handoff/Cargo.toml"),
                    "dependencies": [
                        handoff_path_dependency_fixture(
                            "reticulum-node-core",
                            "*",
                            &root.join("crates/node-core"),
                            None,
                        ),
                        handoff_dependency_fixture("embassy-sync", "=0.8.0", None),
                        handoff_dependency_fixture("embassy-futures", "=0.1.2", Some("dev")),
                        handoff_dependency_fixture("rand_core", "=0.6.4", Some("dev")),
                        handoff_dependency_fixture("static_cell", "=2.1.1", Some("dev")),
                    ],
                },
                dispatch_package_fixture(root),
                supervisor_package_fixture(root),
                storage_model_package_fixture(root),
                submission_projector_package_fixture(root),
                storage_journal_package_fixture(root),
                storage_actor_package_fixture(root),
                device_api_adapter_package_fixture(root),
                radio_tx_dispatch_package_fixture(root),
                lxmf_wire_package_fixture(root),
                radio_interface_package_fixture(root),
                e290_board_facts_package_fixture(root),
                lora_phy_radio_package_fixture(root),
                e290_radio_package_fixture(root),
                device_identity_store_package_fixture(root),
                device_api_credential_store_package_fixture(root),
                announce_clock_package_fixture(root),
                nor_flash_region_package_fixture(root),
                submission_runtime_package_fixture(root),
                rns_inbox_store_package_fixture(root),
            ]
        })
    }

    fn device_identity_store_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-device-identity-store",
            "source": null,
            "manifest_path": root.join("crates/device-identity-store/Cargo.toml"),
            "features": {},
            "dependencies": [
                handoff_dependency_fixture("embedded-storage", "=0.3.1", None),
                handoff_dependency_fixture("rand_core", "=0.6.4", None),
                handoff_dependency_fixture("sha2", "=0.10.9", None),
                handoff_dependency_fixture("zeroize", "=1.9.0", None),
            ],
        })
    }

    fn device_api_credential_store_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-device-api-credential-store",
            "source": null,
            "manifest_path": root.join("crates/device-api-credential-store/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_dependency_fixture("embedded-storage", "=0.3.1", None),
                handoff_path_dependency_fixture(
                    "reticulum-device-api-credentials",
                    "*",
                    &root.join("crates/device-api-credentials"),
                    None,
                ),
                handoff_dependency_fixture("sha2", "=0.10.9", None),
                handoff_dependency_fixture("zeroize", "=1.9.0", None),
            ],
        })
    }

    fn announce_clock_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-announce-clock",
            "source": null,
            "manifest_path": root.join("crates/announce-clock/Cargo.toml"),
            "features": {},
            "dependencies": [
                handoff_dependency_fixture("embedded-storage", "=0.3.1", None),
                handoff_dependency_fixture("sha2", "=0.10.9", None),
            ],
        })
    }

    fn nor_flash_region_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-nor-flash-region",
            "source": null,
            "manifest_path": root.join("crates/nor-flash-region/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_dependency_fixture("embedded-storage", "=0.3.1", None),
            ],
        })
    }

    fn rns_inbox_store_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-rns-inbox-store",
            "source": null,
            "manifest_path": root.join("crates/rns-inbox-store/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_dependency_fixture("embedded-storage", "=0.3.1", None),
                handoff_dependency_fixture("sha2", "=0.10.9", None),
            ],
        })
    }

    fn dispatch_package_fixture(root: &Path) -> serde_json::Value {
        let mut static_cell = handoff_dependency_fixture("static_cell", "=2.1.1", Some("dev"));
        static_cell["uses_default_features"] = serde_json::Value::Bool(true);
        serde_json::json!({
            "name": "reticulum-tx-dispatch",
            "source": null,
            "manifest_path": root.join("crates/tx-dispatch/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_path_dependency_fixture(
                    "reticulum-node-core",
                    "*",
                    &root.join("crates/node-core"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-tx-handoff",
                    "*",
                    &root.join("crates/tx-handoff"),
                    None,
                ),
                handoff_dependency_fixture("embassy-sync", "=0.8.0", None),
                handoff_dependency_fixture("rand_core", "=0.6.4", None),
                handoff_dependency_fixture("sha2", "=0.10.9", None),
                handoff_dependency_fixture("embassy-futures", "=0.1.2", Some("dev")),
                static_cell,
            ],
        })
    }

    fn radio_tx_dispatch_package_fixture(root: &Path) -> serde_json::Value {
        let mut static_cell = handoff_dependency_fixture("static_cell", "=2.1.1", Some("dev"));
        static_cell["uses_default_features"] = serde_json::Value::Bool(true);
        serde_json::json!({
            "name": "reticulum-radio-tx-dispatch",
            "source": null,
            "manifest_path": root.join("crates/radio-tx-dispatch/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_dependency_fixture("embassy-sync", "=0.8.0", None),
                handoff_dependency_fixture("embassy-time", "=0.5.0", None),
                handoff_dependency_fixture("rand_core", "=0.6.4", None),
                handoff_path_dependency_fixture(
                    "reticulum-interface-router",
                    "*",
                    &root.join("crates/interface-router"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-node-core",
                    "*",
                    &root.join("crates/node-core"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-radio-interface",
                    "*",
                    &root.join("crates/radio-interface"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-tx-handoff",
                    "*",
                    &root.join("crates/tx-handoff"),
                    None,
                ),
                handoff_dependency_fixture("embassy-futures", "=0.1.2", Some("dev")),
                static_cell,
            ],
        })
    }

    fn lxmf_wire_package_fixture(root: &Path) -> serde_json::Value {
        let mut ed25519_dalek = handoff_dependency_fixture("ed25519-dalek", "=2.2.0", None);
        ed25519_dalek["features"] = serde_json::json!(["hazmat"]);

        let mut hex = handoff_dependency_fixture("hex", "=0.4.3", Some("dev"));
        hex["uses_default_features"] = serde_json::Value::Bool(true);
        let mut serde = handoff_dependency_fixture("serde", "=1.0.228", Some("dev"));
        serde["uses_default_features"] = serde_json::Value::Bool(true);
        serde["features"] = serde_json::json!(["derive"]);
        let mut serde_json = handoff_dependency_fixture("serde_json", "=1.0.149", Some("dev"));
        serde_json["uses_default_features"] = serde_json::Value::Bool(true);

        serde_json::json!({
            "name": "reticulum-lxmf-wire",
            "source": null,
            "manifest_path": root.join("crates/lxmf-wire/Cargo.toml"),
            "features": {},
            "dependencies": [
                handoff_dependency_fixture("curve25519-dalek", "=4.1.3", None),
                ed25519_dalek,
                handoff_dependency_fixture("hkdf", "=0.12.4", None),
                handoff_dependency_fixture("sha2", "=0.10.9", None),
                handoff_dependency_fixture("subtle", "=2.6.1", None),
                hex,
                serde,
                serde_json,
            ],
        })
    }

    fn radio_interface_package_fixture(root: &Path) -> serde_json::Value {
        let mut conformance = handoff_path_dependency_fixture(
            "reticulum-rns-conformance",
            "*",
            &root.join("crates/rns-conformance"),
            None,
        );
        conformance["uses_default_features"] = serde_json::Value::Bool(true);
        serde_json::json!({
            "name": "reticulum-radio-interface",
            "source": null,
            "manifest_path": root.join("crates/radio-interface/Cargo.toml"),
            "features": {},
            "dependencies": [
                handoff_dependency_fixture("lora-modulation", "=0.1.5", None),
                conformance,
                handoff_dependency_fixture("embassy-sync", "=0.8.0", Some("dev")),
            ],
        })
    }

    fn e290_board_facts_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-board-heltec-vision-master-e290",
            "source": null,
            "manifest_path": root.join("crates/board-heltec-vision-master-e290/Cargo.toml"),
            "features": {},
            "dependencies": [
                handoff_path_dependency_fixture(
                    "reticulum-radio-interface",
                    "*",
                    &root.join("crates/radio-interface"),
                    None,
                ),
            ],
        })
    }

    fn lora_phy_radio_package_fixture(root: &Path) -> serde_json::Value {
        let mut embedded_hal_async =
            handoff_dependency_fixture("embedded-hal-async", "=1.0.0", None);
        embedded_hal_async["uses_default_features"] = serde_json::Value::Bool(true);
        let radio_interface = handoff_path_dependency_fixture(
            "reticulum-radio-interface",
            "*",
            &root.join("crates/radio-interface"),
            None,
        );
        serde_json::json!({
            "name": "reticulum-radio-lora-phy",
            "source": null,
            "manifest_path": root.join("crates/radio-lora-phy/Cargo.toml"),
            "features": {},
            "dependencies": [
                handoff_dependency_fixture("critical-section", "=1.2.0", None),
                embedded_hal_async,
                handoff_dependency_fixture("lora-phy", "=3.0.1", None),
                radio_interface,
            ],
        })
    }

    fn e290_radio_package_fixture(root: &Path) -> serde_json::Value {
        let mut embedded_hal = handoff_dependency_fixture("embedded-hal", "=1.0.0", None);
        embedded_hal["uses_default_features"] = serde_json::Value::Bool(true);
        let mut embedded_hal_async =
            handoff_dependency_fixture("embedded-hal-async", "=1.0.0", None);
        embedded_hal_async["uses_default_features"] = serde_json::Value::Bool(true);
        let local = |name: &str, path: &str| {
            handoff_path_dependency_fixture(name, "*", &root.join(path), None)
        };
        let mut critical = handoff_dependency_fixture("critical-section", "=1.2.0", Some("dev"));
        critical["features"] = serde_json::json!(["std"]);
        serde_json::json!({
            "name": "reticulum-board-heltec-vision-master-e290-radio",
            "source": null,
            "manifest_path": root.join("crates/board-heltec-vision-master-e290-radio/Cargo.toml"),
            "features": {},
            "dependencies": [
                embedded_hal,
                embedded_hal_async,
                handoff_dependency_fixture("lora-phy", "=3.0.1", None),
                local(
                    "reticulum-board-heltec-vision-master-e290",
                    "crates/board-heltec-vision-master-e290",
                ),
                local("reticulum-radio-interface", "crates/radio-interface"),
                local("reticulum-radio-lora-phy", "crates/radio-lora-phy"),
                critical,
            ],
        })
    }

    fn reviewed_closure_fixture(
        root: &Path,
        reviewed_closure: &[ReviewedClosurePackage],
    ) -> (serde_json::Value, String) {
        let packages = reviewed_closure
            .iter()
            .copied()
            .map(|package| {
                let (source, manifest_path) = match package.source {
                    ReviewedClosureSource::Local(relative_manifest) => (
                        serde_json::Value::Null,
                        root.join(relative_manifest).display().to_string(),
                    ),
                    ReviewedClosureSource::Registry => (
                        serde_json::Value::String(CRATES_IO_SOURCE.to_owned()),
                        root.join("fixture/registry")
                            .join(format!("{}-{}", package.name, package.version))
                            .join("Cargo.toml")
                            .display()
                            .to_string(),
                    ),
                    ReviewedClosureSource::Git(source) => (
                        serde_json::Value::String(source.to_owned()),
                        root.join("fixture/git")
                            .join(package.name)
                            .join("Cargo.toml")
                            .display()
                            .to_string(),
                    ),
                };
                serde_json::json!({
                    "name": package.name,
                    "version": package.version,
                    "source": source,
                    "manifest_path": manifest_path,
                })
            })
            .collect::<Vec<_>>();
        let mut tree = String::new();
        for package in reviewed_closure.iter().copied() {
            tree.push_str(&reviewed_closure_display(package, root).unwrap());
            tree.push('\t');
            tree.push_str(&package.features.join(","));
            tree.push('\n');
        }
        (serde_json::json!({ "packages": packages }), tree)
    }

    fn radio_tx_dispatch_reviewed_closure_fixture(root: &Path) -> (serde_json::Value, String) {
        reviewed_closure_fixture(root, &RADIO_TX_DISPATCH_REVIEWED_CLOSURE)
    }

    fn lxmf_wire_reviewed_closure_fixture(root: &Path) -> (serde_json::Value, String) {
        reviewed_closure_fixture(root, &LXMF_WIRE_REVIEWED_CLOSURE)
    }

    fn add_registry_closure_fixture_package(
        metadata: &mut serde_json::Value,
        tree: &mut String,
        root: &Path,
        name: &str,
        version: &str,
    ) {
        metadata["packages"]
            .as_array_mut()
            .expect("fixture packages")
            .push(serde_json::json!({
                "name": name,
                "version": version,
                "source": CRATES_IO_SOURCE,
                "manifest_path": root.join("fixture/registry")
                    .join(format!("{name}-{version}"))
                    .join("Cargo.toml"),
            }));
        tree.push_str(&format!("{name} v{version}\t\n"));
    }

    fn add_local_closure_fixture_package(
        metadata: &mut serde_json::Value,
        tree: &mut String,
        root: &Path,
        name: &str,
        version: &str,
        relative_manifest: &str,
    ) {
        let manifest = root.join(relative_manifest);
        metadata["packages"]
            .as_array_mut()
            .expect("fixture packages")
            .push(serde_json::json!({
                "name": name,
                "version": version,
                "source": null,
                "manifest_path": manifest,
            }));
        tree.push_str(&format!(
            "{name} v{version} ({})\t\n",
            manifest
                .parent()
                .expect("fixture manifest parent")
                .display()
        ));
    }

    fn storage_model_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-storage-model",
            "source": null,
            "manifest_path": root.join("crates/storage-model/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_dependency_fixture("minicbor", "=2.2.2", None),
                handoff_dependency_fixture("sha2", "=0.10.9", None),
            ],
        })
    }

    fn storage_journal_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-storage-journal",
            "source": null,
            "manifest_path": root.join("crates/storage-journal/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_dependency_fixture("embedded-storage", "=0.3.1", None),
                handoff_path_dependency_fixture(
                    "reticulum-storage-model",
                    "*",
                    &root.join("crates/storage-model"),
                    None,
                ),
                handoff_dependency_fixture("sha2", "=0.10.9", None),
            ],
        })
    }

    fn storage_actor_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-storage-actor",
            "source": null,
            "manifest_path": root.join("crates/storage-actor/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_dependency_fixture("embedded-storage", "=0.3.1", None),
                handoff_path_dependency_fixture(
                    "reticulum-node-core",
                    "*",
                    &root.join("crates/node-core"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-storage-journal",
                    "*",
                    &root.join("crates/storage-journal"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-storage-model",
                    "*",
                    &root.join("crates/storage-model"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-submission-projector",
                    "*",
                    &root.join("crates/submission-projector"),
                    None,
                ),
                handoff_dependency_fixture("rand_core", "=0.6.4", Some("dev")),
            ],
        })
    }

    fn submission_runtime_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-submission-runtime",
            "source": null,
            "manifest_path": root.join("crates/submission-runtime/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_dependency_fixture("embassy-sync", "=0.8.0", None),
                handoff_dependency_fixture("embedded-storage", "=0.3.1", None),
                handoff_dependency_fixture("rand_core", "=0.6.4", None),
                handoff_path_dependency_fixture(
                    "reticulum-node-core",
                    "*",
                    &root.join("crates/node-core"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-storage-actor",
                    "*",
                    &root.join("crates/storage-actor"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-storage-model",
                    "*",
                    &root.join("crates/storage-model"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-submission-projector",
                    "*",
                    &root.join("crates/submission-projector"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-tx-supervisor",
                    "*",
                    &root.join("crates/tx-supervisor"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-storage-journal",
                    "*",
                    &root.join("crates/storage-journal"),
                    Some("dev"),
                ),
            ],
        })
    }

    fn fixture_package_mut<'a>(
        metadata: &'a mut serde_json::Value,
        name: &str,
    ) -> &'a mut serde_json::Value {
        metadata["packages"]
            .as_array_mut()
            .expect("fixture packages array")
            .iter_mut()
            .find(|package| package["name"].as_str() == Some(name))
            .unwrap_or_else(|| panic!("fixture package {name}"))
    }

    fn fixture_dependency_mut<'a>(
        package: &'a mut serde_json::Value,
        name: &str,
        kind: Option<&str>,
    ) -> &'a mut serde_json::Value {
        package["dependencies"]
            .as_array_mut()
            .expect("fixture dependencies array")
            .iter_mut()
            .find(|dependency| {
                dependency["name"].as_str() == Some(name)
                    && match kind {
                        Some(kind) => dependency["kind"].as_str() == Some(kind),
                        None => dependency["kind"].is_null(),
                    }
            })
            .unwrap_or_else(|| panic!("fixture dependency {name} with kind {kind:?}"))
    }

    fn device_api_adapter_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-device-api-adapter",
            "source": null,
            "manifest_path": root.join("crates/device-api-adapter/Cargo.toml"),
            "features": {
                "default": [],
                "experimental-rns-data": ["reticulum-device-api/experimental-rns-data"],
                "experimental-rns-inbox": ["reticulum-device-api/experimental-rns-inbox"],
            },
            "dependencies": [
                handoff_dependency_fixture("embedded-storage", "=0.3.1", Some("dev")),
                handoff_path_dependency_fixture(
                    "reticulum-device-api",
                    "*",
                    &root.join("crates/device-api"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-storage-actor",
                    "*",
                    &root.join("crates/storage-actor"),
                    Some("dev"),
                ),
                handoff_path_dependency_fixture(
                    "reticulum-storage-model",
                    "*",
                    &root.join("crates/storage-model"),
                    None,
                ),
            ],
        })
    }

    fn device_api_edge_metadata_fixture(root: &Path) -> serde_json::Value {
        let mut session_credentials = handoff_path_dependency_fixture(
            "reticulum-device-api-credentials",
            "*",
            &root.join("crates/device-api-credentials"),
            None,
        );
        session_credentials["uses_default_features"] = serde_json::Value::Bool(true);
        let mut session_framing = handoff_path_dependency_fixture(
            "reticulum-device-api-framing",
            "*",
            &root.join("crates/device-api-framing"),
            None,
        );
        session_framing["uses_default_features"] = serde_json::Value::Bool(true);
        let mut session_handoff = handoff_path_dependency_fixture(
            "reticulum-device-api-handoff",
            "*",
            &root.join("crates/device-api-handoff"),
            None,
        );
        session_handoff["uses_default_features"] = serde_json::Value::Bool(true);
        let mut session_hex = handoff_dependency_fixture("hex", "=0.4.3", Some("dev"));
        session_hex["uses_default_features"] = serde_json::Value::Bool(true);
        let mut session_adapter = handoff_path_dependency_fixture(
            "reticulum-device-api-adapter",
            "*",
            &root.join("crates/device-api-adapter"),
            Some("dev"),
        );
        session_adapter["uses_default_features"] = serde_json::Value::Bool(true);
        session_adapter["features"] = serde_json::json!(["experimental-rns-data"]);
        let mut session_storage = handoff_path_dependency_fixture(
            "reticulum-storage-model",
            "*",
            &root.join("crates/storage-model"),
            Some("dev"),
        );
        session_storage["uses_default_features"] = serde_json::Value::Bool(true);
        let mut pairing_hex = handoff_dependency_fixture("hex", "=0.4.3", Some("dev"));
        pairing_hex["uses_default_features"] = serde_json::Value::Bool(true);

        serde_json::json!({
            "packages": [
                {
                    "name": "reticulum-device-api-framing",
                    "source": null,
                    "manifest_path": root.join("crates/device-api-framing/Cargo.toml"),
                    "features": {},
                    "dependencies": [
                        handoff_dependency_fixture("zeroize", "=1.9.0", None),
                    ],
                },
                {
                    "name": "reticulum-device-api-pairing-control",
                    "source": null,
                    "manifest_path": root.join("crates/device-api-pairing-control/Cargo.toml"),
                    "features": {},
                    "dependencies": [
                        handoff_path_dependency_fixture(
                            "reticulum-device-api-framing",
                            "*",
                            &root.join("crates/device-api-framing"),
                            None,
                        ),
                    ],
                },
                {
                    "name": "reticulum-device-api-handoff",
                    "source": null,
                    "manifest_path": root.join("crates/device-api-handoff/Cargo.toml"),
                    "features": {},
                    "dependencies": [
                        handoff_dependency_fixture("embassy-sync", "=0.8.0", None),
                        handoff_path_dependency_fixture(
                            "reticulum-device-api",
                            "*",
                            &root.join("crates/device-api"),
                            None,
                        ),
                    ],
                },
                {
                    "name": "reticulum-device-api-credentials",
                    "source": null,
                    "manifest_path": root.join("crates/device-api-credentials/Cargo.toml"),
                    "features": {},
                    "dependencies": [
                        handoff_path_dependency_fixture(
                            "reticulum-device-api",
                            "*",
                            &root.join("crates/device-api"),
                            None,
                        ),
                        handoff_dependency_fixture("subtle", "=2.6.1", None),
                        handoff_dependency_fixture("zeroize", "=1.9.0", None),
                    ],
                },
                {
                    "name": "reticulum-device-api-pairing-policy",
                    "source": null,
                    "manifest_path": root.join("crates/device-api-pairing-policy/Cargo.toml"),
                    "features": { "default": [] },
                    "dependencies": [
                        handoff_path_dependency_fixture(
                            "reticulum-device-api-credentials",
                            "*",
                            &root.join("crates/device-api-credentials"),
                            None,
                        ),
                    ],
                },
                {
                    "name": "reticulum-device-api-pairing",
                    "source": null,
                    "manifest_path": root.join("crates/device-api-pairing/Cargo.toml"),
                    "features": {},
                    "dependencies": [
                        handoff_dependency_fixture("hmac", "=0.12.1", None),
                        handoff_path_dependency_fixture(
                            "reticulum-device-api-credentials",
                            "*",
                            &root.join("crates/device-api-credentials"),
                            None,
                        ),
                        handoff_path_dependency_fixture(
                            "reticulum-device-api-framing",
                            "*",
                            &root.join("crates/device-api-framing"),
                            None,
                        ),
                        handoff_dependency_fixture("sha2", "=0.10.9", None),
                        handoff_dependency_fixture("zeroize", "=1.9.0", None),
                        pairing_hex,
                    ],
                },
                {
                    "name": "reticulum-device-api-session",
                    "source": null,
                    "manifest_path": root.join("crates/device-api-session/Cargo.toml"),
                    "features": {},
                    "dependencies": [
                        handoff_dependency_fixture("hkdf", "=0.12.4", None),
                        handoff_dependency_fixture("hmac", "=0.12.1", None),
                        handoff_dependency_fixture("rand_core", "=0.6.4", None),
                        handoff_path_dependency_fixture(
                            "reticulum-device-api",
                            "*",
                            &root.join("crates/device-api"),
                            None,
                        ),
                        session_credentials,
                        session_framing,
                        session_handoff,
                        handoff_dependency_fixture("sha2", "=0.10.9", None),
                        handoff_dependency_fixture("zeroize", "=1.9.0", None),
                        session_hex,
                        session_adapter,
                        session_storage,
                    ],
                },
            ],
        })
    }

    fn storage_hil_metadata_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "packages": [{
                "name": "reticulum-heltec-tracker-v2-storage-hil",
                "source": null,
                "manifest_path": root.join("firmware/heltec-tracker-v2-storage-hil/Cargo.toml"),
                "features": {},
                "dependencies": [
                    hil_registry_dependency_fixture("embedded-storage", "=0.3.1", &[]),
                    hil_registry_dependency_fixture(
                        "esp-backtrace",
                        "=0.19.0",
                        &["esp32s3", "panic-handler", "println"],
                    ),
                    hil_registry_dependency_fixture(
                        "esp-bootloader-esp-idf",
                        "=0.5.0",
                        &["esp32s3", "log-04", "validation"],
                    ),
                    hil_registry_dependency_fixture(
                        "esp-hal",
                        "=1.1.1",
                        &[
                            "esp32s3",
                            "exception-handler",
                            "float-save-restore",
                            "log-04",
                            "rt",
                            "unstable",
                        ],
                    ),
                    hil_registry_dependency_fixture(
                        "esp-println",
                        "=0.17.0",
                        &["auto", "esp32s3", "log-04"],
                    ),
                    hil_registry_dependency_fixture(
                        "esp-storage",
                        "=0.9.0",
                        &["critical-section", "esp32s3"],
                    ),
                    hil_registry_dependency_fixture("log", "=0.4.27", &[]),
                    handoff_path_dependency_fixture(
                        "reticulum-nor-flash-region",
                        "*",
                        &root.join("crates/nor-flash-region"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-storage-journal",
                        "*",
                        &root.join("crates/storage-journal"),
                        None,
                    ),
                    handoff_path_dependency_fixture(
                        "reticulum-storage-model",
                        "*",
                        &root.join("crates/storage-model"),
                        None,
                    ),
                ],
            }],
        })
    }

    fn hil_registry_dependency_fixture(
        name: &str,
        requirement: &str,
        features: &[&str],
    ) -> serde_json::Value {
        let mut dependency = handoff_dependency_fixture(name, requirement, None);
        dependency["features"] = serde_json::json!(features);
        dependency
    }

    fn submission_projector_package_fixture(root: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": "reticulum-submission-projector",
            "source": null,
            "manifest_path": root.join("crates/submission-projector/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_path_dependency_fixture(
                    "reticulum-node-core",
                    "*",
                    &root.join("crates/node-core"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-storage-model",
                    "*",
                    &root.join("crates/storage-model"),
                    None,
                ),
                handoff_dependency_fixture("rand_core", "=0.6.4", Some("dev")),
                handoff_path_dependency_fixture(
                    "reticulum-rns-rete",
                    "*",
                    &root.join("crates/rns-rete"),
                    Some("dev"),
                ),
            ],
        })
    }

    fn supervisor_package_fixture(root: &Path) -> serde_json::Value {
        let mut static_cell = handoff_dependency_fixture("static_cell", "=2.1.1", Some("dev"));
        static_cell["uses_default_features"] = serde_json::Value::Bool(true);
        serde_json::json!({
            "name": "reticulum-tx-supervisor",
            "source": null,
            "manifest_path": root.join("crates/tx-supervisor/Cargo.toml"),
            "features": { "default": [] },
            "dependencies": [
                handoff_path_dependency_fixture(
                    "reticulum-node-core",
                    "*",
                    &root.join("crates/node-core"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-tx-dispatch",
                    "*",
                    &root.join("crates/tx-dispatch"),
                    None,
                ),
                handoff_dependency_fixture("embassy-sync", "=0.8.0", None),
                handoff_dependency_fixture("embassy-futures", "=0.1.2", None),
                handoff_dependency_fixture("embassy-time", "=0.5.0", None),
                handoff_dependency_fixture("rand_core", "=0.6.4", None),
                handoff_path_dependency_fixture(
                    "reticulum-interface-router",
                    "*",
                    &root.join("crates/interface-router"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-tx-handoff",
                    "*",
                    &root.join("crates/tx-handoff"),
                    None,
                ),
                handoff_path_dependency_fixture(
                    "reticulum-rns-rete",
                    "*",
                    &root.join("crates/rns-rete"),
                    Some("dev"),
                ),
                static_cell,
            ],
        })
    }

    fn tracker_radio_metadata_fixture(root: &Path) -> serde_json::Value {
        let critical = handoff_dependency_fixture("critical-section", "=1.2.0", None);
        let mut embedded_hal = handoff_dependency_fixture("embedded-hal", "=1.0.0", None);
        embedded_hal["uses_default_features"] = serde_json::Value::Bool(true);
        let mut embedded_hal_async =
            handoff_dependency_fixture("embedded-hal-async", "=1.0.0", None);
        embedded_hal_async["uses_default_features"] = serde_json::Value::Bool(true);
        let lora_phy = handoff_dependency_fixture("lora-phy", "=3.0.1", None);
        let mut board = handoff_path_dependency_fixture(
            "reticulum-board-heltec-tracker-v2",
            "*",
            &root.join("crates/board-heltec-tracker-v2"),
            None,
        );
        board["uses_default_features"] = serde_json::Value::Bool(true);
        let mut framing = handoff_path_dependency_fixture(
            "reticulum-radio-interface",
            "*",
            &root.join("crates/radio-interface"),
            None,
        );
        framing["uses_default_features"] = serde_json::Value::Bool(true);
        let shared = handoff_path_dependency_fixture(
            "reticulum-radio-lora-phy",
            "*",
            &root.join("crates/radio-lora-phy"),
            None,
        );
        let mut dev_critical =
            handoff_dependency_fixture("critical-section", "=1.2.0", Some("dev"));
        dev_critical["features"] = serde_json::json!(["std"]);
        let mut facade_radio = handoff_path_dependency_fixture(
            "reticulum-board-heltec-tracker-v2-radio",
            "*",
            &root.join("crates/board-heltec-tracker-v2-radio"),
            None,
        );
        facade_radio["uses_default_features"] = serde_json::Value::Bool(true);

        serde_json::json!({
            "packages": [
                {
                    "name": "reticulum-board-heltec-tracker-v2-radio",
                    "source": null,
                    "manifest_path": root.join("crates/board-heltec-tracker-v2-radio/Cargo.toml"),
                    "features": {
                        "default": [],
                        "near-field-attenuation": [],
                    },
                    "dependencies": [
                        critical,
                        embedded_hal,
                        embedded_hal_async,
                        lora_phy,
                        board,
                        framing,
                        shared,
                        dev_critical,
                    ],
                },
                {
                    "name": "reticulum-board-heltec-tracker-v2-tx-hil",
                    "source": null,
                    "manifest_path": root.join("crates/board-heltec-tracker-v2-tx-hil/Cargo.toml"),
                    "features": {
                        "default": [],
                        "near-field-attenuation-hil": [
                            "reticulum-board-heltec-tracker-v2-radio/near-field-attenuation"
                        ],
                    },
                    "dependencies": [facade_radio],
                },
            ],
        })
    }

    fn handoff_dependency_fixture(
        name: &str,
        requirement: &str,
        kind: Option<&str>,
    ) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "source": "registry+https://github.com/rust-lang/crates.io-index",
            "req": requirement,
            "kind": kind,
            "rename": null,
            "optional": false,
            "uses_default_features": false,
            "features": [],
            "target": null,
            "path": null,
        })
    }

    fn handoff_path_dependency_fixture(
        name: &str,
        requirement: &str,
        path: &Path,
        kind: Option<&str>,
    ) -> serde_json::Value {
        let mut dependency = handoff_dependency_fixture(name, requirement, kind);
        dependency["source"] = serde_json::Value::Null;
        dependency["path"] = serde_json::Value::String(path.display().to_string());
        dependency
    }

    fn portable_dependency_fixture(name: &str) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "path": null,
        })
    }

    fn dependency_fixture(name: &str, path: &Path) -> serde_json::Value {
        serde_json::json!({
            "name": name,
            "kind": null,
            "optional": false,
            "rename": null,
            "source": null,
            "target": null,
            "path": path,
        })
    }

    fn optional_dependency_fixture(name: &str, path: &Path) -> serde_json::Value {
        let mut dependency = dependency_fixture(name, path);
        dependency["optional"] = serde_json::Value::Bool(true);
        dependency
    }

    fn package_fixture(id: &str, name: &str, path: &Path) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "name": name,
            "source": null,
            "manifest_path": path.join("Cargo.toml"),
        })
    }

    fn resolved_dependency_fixture(package_id: &str) -> serde_json::Value {
        serde_json::json!({
            "pkg": package_id,
            "dep_kinds": [{ "kind": null, "target": null }],
        })
    }

    #[test]
    fn api_surface_records_new_public_methods_fields_and_trait_escapes() {
        let file = syn::parse_file(
            r#"
            pub struct TrackerRxRadio<T> { pub inner: T }
            impl<T> TrackerRxRadio<T> {
                pub fn receive(&mut self) {}
                pub fn into_inner(self) -> T { self.inner }
            }
            impl<T> core::ops::Deref for TrackerRxRadio<T> {
                type Target = T;
                fn deref(&self) -> &T { &self.inner }
            }
            "#,
        )
        .unwrap();
        let mut surface = Vec::new();
        collect_public_file_api("board", &file, &mut surface);

        assert!(
            surface
                .iter()
                .any(|line| line.contains("field inner pub T"))
        );
        assert!(surface.iter().any(|line| line.contains("fn into_inner")));
        assert!(
            surface
                .iter()
                .any(|line| line.contains("impl core :: ops :: Deref for TrackerRxRadio"))
        );
    }

    #[test]
    fn api_surface_records_impls_outside_the_public_type_file() {
        let declaration = syn::parse_file("pub struct TrackerRxRadio<T>(T);").unwrap();
        let extension = syn::parse_file(
            r#"
            mod private_extension {
                impl<T> super::TrackerRxRadio<T> {
                    pub fn raw_radio(&mut self) {}
                }
                impl<T> AsMut<T> for super::TrackerRxRadio<T> {
                    fn as_mut(&mut self) -> &mut T { &mut self.0 }
                }
            }
            "#,
        )
        .unwrap();
        let mut public_types = BTreeSet::new();
        collect_public_type_names(&declaration.items, &mut public_types);
        collect_public_type_names(&extension.items, &mut public_types);

        let mut surface = Vec::new();
        collect_public_file_api_with_types("decl", &declaration, &public_types, &mut surface);
        collect_public_file_api_with_types("ext", &extension, &public_types, &mut surface);

        assert!(surface.iter().any(|line| line.contains("fn raw_radio")));
        assert!(
            surface
                .iter()
                .any(|line| line.contains("impl AsMut < T > for super :: TrackerRxRadio < T >"))
        );
    }

    #[test]
    fn api_surface_records_dependency_module_trait_and_macro_escapes() {
        let file = syn::parse_file(
            r#"
            pub extern crate lora_phy as radio;
            pub mod escape {
                pub trait RawOwner { fn raw(&self); }
            }
            #[macro_export]
            macro_rules! expose_inner { () => {}; }
            "#,
        )
        .unwrap();
        let mut surface = Vec::new();
        collect_public_file_api("board", &file, &mut surface);

        assert!(
            surface
                .iter()
                .any(|line| line.contains("extern crate lora_phy as radio"))
        );
        assert!(surface.iter().any(|line| line.contains("mod escape")));
        assert!(surface.iter().any(|line| line.contains("trait RawOwner")));
        assert!(
            surface
                .iter()
                .any(|line| line.contains("exported-macro expose_inner"))
        );
        assert!(
            surface
                .iter()
                .any(|line| line.contains("item-macro expose_inner"))
        );
        assert!(
            surface
                .iter()
                .any(|line| line.contains("attribute # [macro_export]"))
        );
    }

    #[test]
    fn api_surface_does_not_change_for_enum_documentation_only() {
        let documented = syn::parse_file(
            r#"
            pub enum Fault {
                /// Documentation is not part of the callable surface.
                Spi,
            }
            "#,
        )
        .unwrap();
        let plain = syn::parse_file("pub enum Fault { Spi }").unwrap();
        let mut documented_surface = Vec::new();
        let mut plain_surface = Vec::new();
        collect_public_file_api("test", &documented, &mut documented_surface);
        collect_public_file_api("test", &plain, &mut plain_surface);
        assert_eq!(documented_surface, plain_surface);
    }
}
