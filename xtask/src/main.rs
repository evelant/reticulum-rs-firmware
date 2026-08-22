//! Small, product-focused build helpers for the E290 firmware.

use object::{
    Architecture, BinaryFormat, Object, ObjectSection, ObjectSymbol, SectionKind, SymbolKind,
    SymbolSection,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
};

const FIRMWARE_PACKAGE: &str = "reticulum-e290-firmware";
const FIRMWARE_BINARY: &str = "reticulum-e290-firmware";
const TARGET: &str = "xtensa-esp32s3-none-elf";
const PARTITION_TABLE: &str = "partitions/e290.csv";
const ROLLBACK_BOOTLOADER: &str = "target/e290-bootloader/bootloader/bootloader.bin";
const STACK_GUARD_OFFSET_BYTES: u64 = 60;
const STACK_GUARD_BYTES: u64 = size_of::<u32>() as u64;
// The reviewed powered credential-boot failure needed roughly 38 KiB beyond
// the compiler's largest single frame: synchronous mount/classification,
// executor and ROM memcpy frames are cumulative even though `.stack_sizes`
// reports them one at a time. Round that observed nested use up to 40 KiB and
// retain another 8 KiB for interrupt entry and future bounded growth. This
// address-independent gate covers both formatted materialization and
// unformatted-media classification.
const REVIEWED_NESTED_STARTUP_STACK_BYTES: u64 = 40 * 1024;
const MINIMUM_STACK_RESIDUAL_BYTES: u64 = 8 * 1024;
const MINIMUM_STACK_HEADROOM_BYTES: u64 =
    REVIEWED_NESTED_STARTUP_STACK_BYTES + MINIMUM_STACK_RESIDUAL_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Profile {
    Appliance,
    Gateway,
}

impl Profile {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "appliance" => Ok(Self::Appliance),
            "gateway" => Ok(Self::Gateway),
            _ => Err(format!(
                "unknown profile {value:?}; expected appliance or gateway"
            )),
        }
    }

    const fn feature(self) -> &'static str {
        match self {
            Self::Appliance => "appliance",
            Self::Gateway => "gateway",
        }
    }

    const fn directory(self) -> &'static str {
        match self {
            Self::Appliance => "e290-appliance",
            Self::Gateway => "e290-gateway",
        }
    }
}

#[derive(Debug)]
enum Task {
    Doctor,
    Build {
        profile: Profile,
    },
    Package {
        profile: Profile,
        output: Option<PathBuf>,
    },
    CheckElf {
        profile: Profile,
        elf: Option<PathBuf>,
    },
    Help,
}

fn main() -> ExitCode {
    match parse_task(env::args().skip(1).collect()).and_then(|task| run(task, &workspace_root())) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask must be directly below the workspace root")
        .to_owned()
}

fn parse_task(arguments: Vec<String>) -> Result<Task, String> {
    let Some(command) = arguments.first().map(String::as_str) else {
        return Ok(Task::Help);
    };
    let tail = &arguments[1..];
    match command {
        "doctor" if tail.is_empty() => Ok(Task::Doctor),
        "build" => Ok(Task::Build {
            profile: parse_profile(tail)?,
        }),
        "package" => {
            let (profile, output) = parse_profile_and_path(tail, "--output")?;
            Ok(Task::Package { profile, output })
        }
        "check-elf" => {
            let (profile, elf) = parse_profile_and_path(tail, "--elf")?;
            Ok(Task::CheckElf { profile, elf })
        }
        "help" | "--help" | "-h" if tail.is_empty() => Ok(Task::Help),
        _ => Err(format!("invalid command or arguments\n\n{}", usage())),
    }
}

fn parse_profile(arguments: &[String]) -> Result<Profile, String> {
    let (profile, path) = parse_profile_and_path(arguments, "--unused")?;
    if path.is_some() {
        return Err("unexpected path option".to_owned());
    }
    Ok(profile)
}

fn parse_profile_and_path(
    arguments: &[String],
    path_option: &str,
) -> Result<(Profile, Option<PathBuf>), String> {
    let mut profile = Profile::Gateway;
    let mut path = None;
    let mut index = 0;
    while index < arguments.len() {
        let option = arguments[index].as_str();
        let value = arguments
            .get(index + 1)
            .ok_or_else(|| format!("{option} requires a value"))?;
        match option {
            "--profile" => profile = Profile::parse(value)?,
            option if option == path_option => path = Some(PathBuf::from(value)),
            _ => return Err(format!("unknown option {option:?}")),
        }
        index += 2;
    }
    Ok((profile, path))
}

fn run(task: Task, workspace: &Path) -> Result<(), String> {
    match task {
        Task::Doctor => doctor(workspace),
        Task::Build { profile } => build(workspace, profile),
        Task::Package { profile, output } => package(workspace, profile, output),
        Task::CheckElf { profile, elf } => {
            let elf = elf.unwrap_or_else(|| firmware_elf(workspace, profile));
            check_elf(&elf)
        }
        Task::Help => {
            println!("{}", usage());
            Ok(())
        }
    }
}

fn usage() -> &'static str {
    "E290 project commands:\n\
     cargo run --locked -p xtask -- doctor\n\
     cargo run --locked -p xtask -- build [--profile appliance|gateway]\n\
     cargo run --locked -p xtask -- package [--profile appliance|gateway] [--output PATH]\n\
     cargo run --locked -p xtask -- check-elf [--profile appliance|gateway] [--elf PATH]"
}

fn doctor(workspace: &Path) -> Result<(), String> {
    for (program, arguments) in [
        ("rustc", &["+esp", "--version"][..]),
        ("cargo", &["+esp", "--version"][..]),
        ("xtensa-esp32s3-elf-gcc", &["--version"][..]),
        ("espflash", &["--version"][..]),
    ] {
        run_command(workspace, program, arguments, &[])?;
    }
    for required in ["firmware/e290/Cargo.toml", PARTITION_TABLE] {
        if !workspace.join(required).is_file() {
            return Err(format!("required workspace file {required} is missing"));
        }
    }
    println!("toolchain and workspace layout are ready");
    Ok(())
}

fn build(workspace: &Path, profile: Profile) -> Result<(), String> {
    let target_dir = target_directory(workspace, profile);
    let target_dir_value = target_dir.to_string_lossy().into_owned();
    let arguments = [
        "+esp",
        "build",
        "--release",
        "--locked",
        "--package",
        FIRMWARE_PACKAGE,
        "--target",
        TARGET,
        "--no-default-features",
        "--features",
        profile.feature(),
    ];
    run_command(
        workspace,
        "cargo",
        &arguments,
        &[("CARGO_TARGET_DIR", target_dir_value.as_str())],
    )?;
    check_elf(&firmware_elf(workspace, profile))
}

fn package(workspace: &Path, profile: Profile, output: Option<PathBuf>) -> Result<(), String> {
    let bootloader = workspace.join(ROLLBACK_BOOTLOADER);
    match fs::metadata(&bootloader) {
        Ok(metadata) if metadata.is_file() && metadata.len() != 0 => {}
        _ => {
            return Err(format!(
                "rollback-enabled bootloader is missing at {}; run firmware/e290/bootloader/build-container.sh first",
                bootloader.display()
            ));
        }
    }
    build(workspace, profile)?;
    let elf = firmware_elf(workspace, profile);
    let output = output.unwrap_or_else(|| {
        target_directory(workspace, profile).join(format!("e290-{}.bin", profile.feature()))
    });
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "could not create output directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let elf_value = elf.to_string_lossy().into_owned();
    let output_value = output.to_string_lossy().into_owned();
    let partition_value = workspace
        .join(PARTITION_TABLE)
        .to_string_lossy()
        .into_owned();
    let bootloader_value = bootloader.to_string_lossy().into_owned();
    let arguments = [
        "save-image",
        "--skip-update-check",
        "--chip",
        "esp32s3",
        "--merge",
        "--skip-padding",
        "--flash-mode",
        "dio",
        "--flash-freq",
        "80mhz",
        "--flash-size",
        "16mb",
        "--xtal-freq",
        "40mhz",
        "--bootloader",
        bootloader_value.as_str(),
        "--partition-table",
        partition_value.as_str(),
        "--target-app-partition",
        "ota_0",
        elf_value.as_str(),
        output_value.as_str(),
    ];
    run_command(workspace, "espflash", &arguments, &[])?;
    println!("packaged {}", output.display());
    Ok(())
}

fn target_directory(workspace: &Path, profile: Profile) -> PathBuf {
    workspace.join("target").join(profile.directory())
}

fn firmware_elf(workspace: &Path, profile: Profile) -> PathBuf {
    target_directory(workspace, profile)
        .join(TARGET)
        .join("release")
        .join(FIRMWARE_BINARY)
}

fn run_command(
    workspace: &Path,
    program: &str,
    arguments: &[&str],
    environment: &[(&str, &str)],
) -> Result<(), String> {
    let mut command = Command::new(program);
    command.current_dir(workspace).args(arguments);
    for (name, value) in environment {
        command.env(name, value);
    }
    let status = command
        .status()
        .map_err(|error| format!("could not run {program}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

fn check_elf(path: &Path) -> Result<(), String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("could not read firmware ELF {}: {error}", path.display()))?;
    let file = object::File::parse(bytes.as_slice())
        .map_err(|error| format!("could not parse firmware ELF {}: {error}", path.display()))?;
    if file.format() != BinaryFormat::Elf || file.architecture() != Architecture::Xtensa {
        return Err(format!(
            "{} is not a final Xtensa ELF (format={:?}, architecture={:?})",
            path.display(),
            file.format(),
            file.architecture()
        ));
    }

    let stack_end = absolute_symbol(&file, path, "_stack_end_cpu0")?;
    let stack_guard = absolute_symbol(&file, path, "__stack_chk_guard")?;
    let stack_start = absolute_symbol(&file, path, "_stack_start_cpu0")?;
    if stack_guard.checked_sub(stack_end) != Some(STACK_GUARD_OFFSET_BYTES) {
        return Err(format!(
            "unexpected CPU0 stack guard layout: end=0x{stack_end:x} guard=0x{stack_guard:x}"
        ));
    }
    let usable_start = stack_guard
        .checked_add(STACK_GUARD_BYTES)
        .ok_or_else(|| "stack guard address overflow".to_owned())?;
    let usable_stack = stack_start
        .checked_sub(usable_start)
        .ok_or_else(|| "CPU0 stack symbols are not monotonically ordered".to_owned())?;

    let (record_count, maximum_frame) = stack_size_inventory(&file, path)?;
    let required = minimum_required_stack(maximum_frame)?;
    if usable_stack < required {
        return Err(format!(
            "CPU0 usable stack {usable_stack} bytes is smaller than largest compiler frame {maximum_frame} plus {MINIMUM_STACK_HEADROOM_BYTES}-byte reviewed nested-startup headroom"
        ));
    }

    let supervisor_statics: Vec<_> = file
        .symbols()
        .filter(|symbol| {
            symbol.kind() == SymbolKind::Data
                && symbol.section() != SymbolSection::Undefined
                && symbol.name().is_ok_and(|name| name.ends_with("SUPERVISOR"))
        })
        .filter_map(|symbol| symbol.name().ok())
        .collect();
    if !supervisor_statics.is_empty() {
        return Err(format!(
            "firmware retains forbidden internal supervisor statics: {}",
            supervisor_statics.join(", ")
        ));
    }

    println!(
        "ELF OK: {} stack records, max frame {} bytes, usable CPU0 stack {} bytes, policy headroom {} bytes",
        record_count,
        maximum_frame,
        usable_stack,
        usable_stack - maximum_frame
    );
    Ok(())
}

fn minimum_required_stack(maximum_frame: u64) -> Result<u64, String> {
    maximum_frame
        .checked_add(MINIMUM_STACK_HEADROOM_BYTES)
        .ok_or_else(|| "stack policy calculation overflow".to_owned())
}

fn absolute_symbol(file: &object::File<'_>, path: &Path, name: &str) -> Result<u64, String> {
    let mut matches = file.symbols().filter(|symbol| {
        symbol.name().is_ok_and(|candidate| candidate == name)
            && symbol.section() != SymbolSection::Undefined
    });
    let symbol = matches
        .next()
        .ok_or_else(|| format!("{} has no defined {name} symbol", path.display()))?;
    if matches.next().is_some() {
        return Err(format!(
            "{} has multiple defined {name} symbols",
            path.display()
        ));
    }
    if symbol.section() != SymbolSection::Absolute {
        return Err(format!(
            "{} {name} is not an absolute linker symbol",
            path.display()
        ));
    }
    Ok(symbol.address())
}

fn stack_size_inventory(file: &object::File<'_>, path: &Path) -> Result<(u64, u64), String> {
    let mut sections = file
        .sections()
        .filter(|section| section.name().is_ok_and(|name| name == ".stack_sizes"));
    let section = sections
        .next()
        .ok_or_else(|| format!("{} has no .stack_sizes section", path.display()))?;
    if sections.next().is_some() {
        return Err(format!(
            "{} has multiple .stack_sizes sections",
            path.display()
        ));
    }
    if section.kind() == SectionKind::Unknown || section.relocations().next().is_some() {
        return Err(format!(
            "{} has an invalid final .stack_sizes section",
            path.display()
        ));
    }
    let data = section
        .data()
        .map_err(|error| format!("could not read {} .stack_sizes: {error}", path.display()))?;
    parse_stack_sizes(data)
}

fn parse_stack_sizes(data: &[u8]) -> Result<(u64, u64), String> {
    if data.is_empty() {
        return Err(".stack_sizes is empty".to_owned());
    }
    let mut offset = 0;
    let mut records = 0_u64;
    let mut maximum = 0_u64;
    while offset < data.len() {
        if data.len() - offset < size_of::<u32>() {
            return Err(".stack_sizes contains a truncated function address".to_owned());
        }
        offset += size_of::<u32>();
        let (frame, consumed) = decode_uleb128(&data[offset..])?;
        offset += consumed;
        records += 1;
        maximum = maximum.max(frame);
    }
    Ok((records, maximum))
}

fn decode_uleb128(bytes: &[u8]) -> Result<(u64, usize), String> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().copied().enumerate() {
        let shift = index * 7;
        if shift >= u64::BITS as usize || (shift == 63 && byte & 0x7e != 0) {
            return Err(".stack_sizes frame size overflows u64".to_owned());
        }
        value |= u64::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err(".stack_sizes contains a truncated frame size".to_owned())
}

#[cfg(test)]
mod tests {
    use super::{
        MINIMUM_STACK_HEADROOM_BYTES, Profile, Task, decode_uleb128, minimum_required_stack,
        parse_task,
    };

    #[test]
    fn commands_default_to_gateway() {
        assert!(matches!(
            parse_task(vec!["build".to_owned()]),
            Ok(Task::Build {
                profile: Profile::Gateway
            })
        ));
    }

    #[test]
    fn package_accepts_profile_and_output_in_either_order() {
        let task = parse_task(vec![
            "package".to_owned(),
            "--output".to_owned(),
            "image.bin".to_owned(),
            "--profile".to_owned(),
            "appliance".to_owned(),
        ]);
        assert!(matches!(
            task,
            Ok(Task::Package {
                profile: Profile::Appliance,
                output: Some(_)
            })
        ));
    }

    #[test]
    fn uleb128_decoder_is_bounded() {
        assert_eq!(decode_uleb128(&[0x80, 0x01]), Ok((128, 2)));
        assert!(decode_uleb128(&[0x80]).is_err());
    }

    #[test]
    fn stack_policy_rejects_the_powered_credential_boot_regression() {
        const REGRESSED_MAXIMUM_FRAME: u64 = 53_104;
        const REGRESSED_USABLE_STACK: u64 = 89_612;

        let required = minimum_required_stack(REGRESSED_MAXIMUM_FRAME)
            .expect("the reviewed stack sizes fit u64");
        assert_eq!(required, 102_256);
        assert_eq!(MINIMUM_STACK_HEADROOM_BYTES, 48 * 1024);
        assert!(REGRESSED_USABLE_STACK < required);
        assert!(minimum_required_stack(u64::MAX).is_err());
    }
}
