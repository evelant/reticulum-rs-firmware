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
                 <doctor|build-tracker|check-rns-vectors|check-rnode-hil-vectors|graph-policy|rx-api-policy|print-rx-api-surface|phase1-rx-hil-artifacts|phase1-rx-closure-artifacts|phase1-rx-powered-evidence>"
            );
            ExitCode::from(2)
        }
    }
}

fn check_rnode_hil_vectors() -> ExitCode {
    let root = workspace_root();
    let python = env::var_os("PYTHON").unwrap_or_else(|| "python3".into());
    let generator = root.join("interop/python/generate_rnode_hil_vectors.py");

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
        .env("PYTHONPATH", root.join("interop/python"))
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
            "reticulum-rns-rete",
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
    for forbidden in ["rete-core", "rete-stack", "rete-transport", "rete-lxmf"] {
        if comparison.contains(forbidden) {
            eprintln!("error: Leviculum comparison graph contains forbidden {forbidden}");
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
        validate_resolved_rete_pin(&json, candidate.source, candidate.revision)
            .map_err(|error| format!("Rete pin/report mismatch: {error}"))?;
        validate_resolved_esp_rtos_patch(&json, &root)
            .map_err(|error| format!("esp-rtos patch boundary: {error}"))?;
        validate_firmware_dependency_boundary(&json, &root)
            .map_err(|error| format!("firmware receive-only dependency boundary: {error}"))?;
        validate_portable_layer_dependency_boundary(&json, &root)
            .map_err(|error| format!("portable layer dependency boundary: {error}"))?;
        validate_tx_handoff_dependency_boundary(&json, &root)
            .map_err(|error| format!("TX handoff dependency boundary: {error}"))
    });
    if let Err(error) = resolved {
        eprintln!("error: {error}");
        failed = true;
    }

    if failed {
        ExitCode::FAILURE
    } else {
        println!(
            "ok: all safe, RX, HIL and all-features all-target product graphs and the Leviculum \
             comparison graph are isolated; the returned-fault hook is feature-exclusive; \
             firmware direct dependencies use only the RX façade and every-feature resolution \
             excludes TX ownership crates; resolved Rete packages match reported \
             source/revision; esp-rtos resolves only to the reviewed local patch and its \
             checked vendor inventory reconstructs the pristine registry source; the device \
             API and node core remain mutually isolated and free of direct platform dependencies; \
             the TX handoff depends only on node-core and Embassy Sync 0.8"
        );
        ExitCode::SUCCESS
    }
}

const PRODUCT_GRAPH_FORBIDDEN: [&str; 5] = [
    "leviculum-core",
    "rete-lxmf",
    "lxmf-rs",
    "reticulum-node-core",
    "reticulum-tx-handoff",
];

fn validate_product_graph_boundary(label: &str, tree: &str) -> Result<(), String> {
    for forbidden in PRODUCT_GRAPH_FORBIDDEN {
        if tree.contains(forbidden) {
            return Err(format!(
                "product {label} all-target graph contains forbidden {forbidden}"
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
            if is_platform_implementation_dependency(dependency, workspace) {
                return Err(format!(
                    "{package_name} directly depends on prohibited platform implementation crate {dependency_name}"
                ));
            }
        }
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
        "reticulum-node-core",
        "reticulum-rns-rete",
        "reticulum-tx-handoff",
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
            && package_names
                .get(package_id)
                .is_some_and(|name| matches!(*name, "reticulum-node-core" | "reticulum-tx-handoff"))
        {
            return Err(format!(
                "firmware resolved graph reaches prohibited TX ownership package {}",
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
            "reticulum-node-core",
            "reticulum-tx-handoff",
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
            ("node-core-id", "reticulum-node-core", "crates/node-core"),
            ("tx-handoff-id", "reticulum-tx-handoff", "crates/tx-handoff"),
        ] {
            let mut transitive = metadata.clone();
            transitive["packages"]
                .as_array_mut()
                .unwrap()
                .push(package_fixture(id, name, &root.join(relative_path)));
            transitive["resolve"]["nodes"][3]["deps"] =
                serde_json::json!([resolved_dependency_fixture(id)]);
            assert!(
                validate_firmware_dependency_boundary(&transitive.to_string(), &root).is_err(),
                "transitive {name} firmware dependency was accepted"
            );
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

        for forbidden in ["reticulum-node-core", "reticulum-tx-handoff"] {
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
    fn portable_layer_boundary_accepts_generic_dependencies_and_node_rete_adapter() {
        let root = workspace_root();
        let metadata = portable_layers_metadata_fixture(&root);

        validate_portable_layer_dependency_boundary(&metadata.to_string(), &root).unwrap();
        validate_tx_handoff_dependency_boundary(&metadata.to_string(), &root).unwrap();

        let mut wrong_path = metadata;
        wrong_path["packages"][0]["manifest_path"] =
            serde_json::Value::String(root.join("elsewhere/Cargo.toml").display().to_string());
        assert!(
            validate_portable_layer_dependency_boundary(&wrong_path.to_string(), &root).is_err()
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
                        portable_dependency_fixture("rand_core"),
                        portable_dependency_fixture("reticulum-rns-rete"),
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
            ]
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
