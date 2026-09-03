//! The `asdf` command-line tool.
//!
//! A companion to the library, mirroring libasdf's own tool: the same
//! sub-commands, the same output. Its `info` rendering is checked byte for
//! byte against upstream's committed expected-output fixtures.

use std::io::Write;
use std::process::ExitCode;

use asdf_core::info::{InfoOptions, render};
use asdf_core::reader::{ChecksumStatus, Reader};

const USAGE: &str = "\
Usage: asdf COMMAND [ARGS...]

A tool for inspecting ASDF files.

Commands:
  info              Print a rendering of an ASDF tree
  dd                Dump data from an ASDF binary block
  verify-checksums  Verify binary block MD5 checksums

Run `asdf COMMAND --help` for help on a sub-command.
";

const INFO_USAGE: &str = "\
Usage: asdf info [OPTIONS] FILENAME

Print a human-readable rendering of an ASDF file's YAML tree, and optionally
information about its binary blocks.

Options:
      --no-tree           Do not show the tree
  -b, --blocks            Show information about the file's binary blocks
      --verify-checksums  Verify each block's MD5 checksum (implies --blocks)
  -h, --help              Show this help
";

const DD_USAGE: &str = "\
Usage: asdf dd [OPTIONS] INPUT [OUTPUT|-]

Dump the data of an ASDF binary block to a file, or to standard output.

Options:
  -b, --block=INDEX  Index of the block to dump (default 0)
      --raw          Dump the block's stored bytes without decompressing
  -h, --help         Show this help
";

const VERIFY_USAGE: &str = "\
Usage: asdf verify-checksums FILENAME

Verify the MD5 checksum of each binary block in the file.

Options:
  -h, --help  Show this help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        eprint!("{USAGE}");
        return ExitCode::FAILURE;
    };

    let rest = &args[1..];
    let result = match command.as_str() {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        "info" => cmd_info(rest),
        "dd" => cmd_dd(rest),
        "verify-checksums" => cmd_verify(rest),
        other => {
            eprintln!("asdf: unknown command {other:?}\n");
            eprint!("{USAGE}");
            return ExitCode::FAILURE;
        }
    };

    match result {
        Ok(code) => code,
        Err(message) => {
            eprintln!("asdf: {message}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_info(args: &[String]) -> Result<ExitCode, String> {
    let mut options = InfoOptions { print_tree: true, ..Default::default() };
    let mut filename = None;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{INFO_USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "--no-tree" => options.print_tree = false,
            "-b" | "--blocks" => options.print_blocks = true,
            "--verify-checksums" => {
                options.verify_checksums = true;
                options.print_blocks = true;
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unrecognised option {other:?}"));
            }
            other => filename = Some(other.to_string()),
        }
    }

    let filename = filename.ok_or("info requires a FILENAME")?;
    let reader = Reader::open(&filename).map_err(|e| format!("{filename}: {e}"))?;
    let text = render(&reader, options).map_err(|e| format!("{filename}: {e}"))?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    out.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    Ok(ExitCode::SUCCESS)
}

fn cmd_dd(args: &[String]) -> Result<ExitCode, String> {
    let mut index = 0usize;
    let mut raw = false;
    let mut positional = Vec::new();
    let mut iter = args.iter().peekable();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{DD_USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "--raw" => raw = true,
            "-b" | "--block" => {
                let value = iter.next().ok_or("--block requires an index")?;
                index = value.parse().map_err(|_| format!("bad block index {value:?}"))?;
            }
            other if other.starts_with("--block=") => {
                let value = &other["--block=".len()..];
                index = value.parse().map_err(|_| format!("bad block index {value:?}"))?;
            }
            other if other.starts_with('-') && other.len() > 1 && other != "-" => {
                return Err(format!("unrecognised option {other:?}"));
            }
            other => positional.push(other.to_string()),
        }
    }

    let input = positional.first().ok_or("dd requires an INPUT file")?;
    let reader = Reader::open(input).map_err(|e| format!("{input}: {e}"))?;

    let data = if raw {
        reader.block_raw(index).map(|d| d.to_vec()).map_err(|e| format!("{input}: {e}"))?
    } else {
        reader.block_data(index).map(|d| d.into_owned()).map_err(|e| format!("{input}: {e}"))?
    };

    match positional.get(1).map(String::as_str) {
        None | Some("-") => {
            let stdout = std::io::stdout();
            stdout.lock().write_all(&data).map_err(|e| e.to_string())?;
        }
        Some(path) => std::fs::write(path, &data).map_err(|e| format!("{path}: {e}"))?,
    }
    Ok(ExitCode::SUCCESS)
}

fn cmd_verify(args: &[String]) -> Result<ExitCode, String> {
    let mut filename = None;
    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{VERIFY_USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unrecognised option {other:?}"));
            }
            other => filename = Some(other.to_string()),
        }
    }

    let filename = filename.ok_or("verify-checksums requires a FILENAME")?;
    let reader = Reader::open(&filename).map_err(|e| format!("{filename}: {e}"))?;

    let mut failed = false;
    for index in 0..reader.block_count() {
        let (status, computed) = reader
            .verify_block_checksum(index)
            .map_err(|e| format!("{filename}: block {index}: {e}"))?;
        let hex: String = computed.iter().map(|b| format!("{b:02x}")).collect();
        match status {
            ChecksumStatus::Valid => println!("block {index}: OK {hex}"),
            ChecksumStatus::Absent => println!("block {index}: no checksum"),
            ChecksumStatus::Invalid => {
                println!("block {index}: FAILED (computed {hex})");
                failed = true;
            }
        }
    }
    Ok(if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS })
}
