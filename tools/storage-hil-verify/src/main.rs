use std::{env, fs, path::PathBuf, process::ExitCode};

use reticulum_storage_hil_verify::verify_dump;

fn main() -> ExitCode {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .unwrap_or_else(|| "reticulum-storage-hil-verify".into());
    let Some(path) = arguments.next().map(PathBuf::from) else {
        eprintln!(
            "usage: {} <retlog-dump.bin>",
            PathBuf::from(program).display()
        );
        return ExitCode::from(2);
    };
    if arguments.next().is_some() {
        eprintln!("error: expected exactly one retlog dump path");
        eprintln!("usage: reticulum-storage-hil-verify <retlog-dump.bin>");
        return ExitCode::from(2);
    }

    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!(
                "storage-hil-dump status=FAIL path={} error={error}",
                path.display()
            );
            return ExitCode::FAILURE;
        }
    };
    let report = match verify_dump(&bytes) {
        Ok(report) => report,
        Err(error) => {
            eprintln!(
                "storage-hil-dump status=FAIL path={} error={error}",
                path.display()
            );
            return ExitCode::FAILURE;
        }
    };

    println!(
        "storage-hil-dump status=PASS path={} bytes={} bank={:?} generation={} committed_records={} occupied_slots={} accepted_submissions={} compaction_pending=false",
        path.display(),
        report.dump_bytes,
        report.bank,
        report.generation,
        report.committed_records,
        report.occupied_slots,
        report.accepted_submissions,
    );
    println!(
        "storage-hil-dump fixture_submission=0x{:016x} revision={} lifecycle=Delivered manifest_a_erased=true bank_b_unused_tail_erased=true bank_b_unused_offset=0x{:x}",
        report.fixture_submission_id.get(),
        report.fixture_revision,
        report.bank_b_unused_offset,
    );
    ExitCode::SUCCESS
}
