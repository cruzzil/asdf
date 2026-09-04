//! The `asdf` command-line tool.
//!
//! A companion to the library, mirroring libasdf's own tool: the same
//! sub-commands, the same output. Its `info` rendering is checked byte for
//! byte against upstream's committed expected-output fixtures.

use std::io::Write;
use std::process::ExitCode;

use asdf_core::events::{EventOptions, events, render_event};
use asdf_core::info::{InfoOptions, render};
use asdf_core::reader::{ChecksumStatus, Reader};

const USAGE: &str = "\
Usage: asdf COMMAND [ARGS...]

A tool for inspecting ASDF files.

Commands:
  info              Print a rendering of an ASDF tree
  dd                Dump data from an ASDF binary block
  events            Print the low-level parser events for a file
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
  -n, --ndarray=PATH Tree path of an ndarray whose block to dump
  -r, --raw          Dump the block's stored bytes without decompressing
  -c, --chunk-size=N Read and write in chunks of N bytes
  -h, --help         Show this help
";

const EVENTS_USAGE: &str = "\
Usage: asdf events [OPTIONS] FILENAME

Print the low-level parser events for an ASDF file: the version headers, any
comments, the block index, the tree's extent and the YAML events inside it,
then each binary block.

Options:
      --no-yaml    Report the tree's extent but not the YAML events inside it
      --cap-tree   Capture the YAML tree and print it with the end-of-tree event
  -v, --verbose    Print each event's details, not just its type
  -h, --help       Show this help
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
        "events" => cmd_events(rest),
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
    let mut ndarray_path: Option<String> = None;
    let mut chunk_size = 0usize;
    let mut positional = Vec::new();
    let mut iter = args.iter().peekable();

    /// Read an option's argument, from `--opt=value` or the next word.
    fn value_of<'a>(
        arg: &'a str,
        prefix: &str,
        iter: &mut impl Iterator<Item = &'a String>,
    ) -> Result<String, String> {
        match arg.strip_prefix(prefix).and_then(|rest| rest.strip_prefix('=')) {
            Some(value) => Ok(value.to_string()),
            None => iter.next().cloned().ok_or(format!("{prefix} requires a value")),
        }
    }

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{DD_USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "-r" | "--raw" => raw = true,
            other if other == "-b" || other.starts_with("--block") => {
                let value = value_of(other, "--block", &mut iter)?;
                index = value.parse().map_err(|_| format!("bad block index {value:?}"))?;
            }
            other if other == "-n" || other.starts_with("--ndarray") => {
                ndarray_path = Some(value_of(other, "--ndarray", &mut iter)?);
            }
            other if other == "-c" || other.starts_with("--chunk-size") => {
                let value = value_of(other, "--chunk-size", &mut iter)?;
                chunk_size = value.parse().map_err(|_| format!("bad chunk size {value:?}"))?;
            }
            other if other.starts_with('-') && other.len() > 1 && other != "-" => {
                return Err(format!("unrecognised option {other:?}"));
            }
            other => positional.push(other.to_string()),
        }
    }

    let input = positional.first().ok_or("dd requires an INPUT file")?;
    let reader = Reader::open(input).map_err(|e| format!("{input}: {e}"))?;

    // `--ndarray` names the array rather than the block, which is how a
    // caller who knows the tree but not the block layout asks for its data.
    if let Some(path) = &ndarray_path {
        index = block_of_ndarray(&reader, path).map_err(|e| format!("{input}: {e}"))?;
    }

    let data = if raw {
        reader.block_raw(index).map(|d| d.to_vec()).map_err(|e| format!("{input}: {e}"))?
    } else {
        reader.block_data(index).map(|d| d.into_owned()).map_err(|e| format!("{input}: {e}"))?
    };

    match positional.get(1).map(String::as_str) {
        None | Some("-") => write_chunked(&mut std::io::stdout().lock(), &data, chunk_size)?,
        Some(path) => {
            let file = std::fs::File::create(path).map_err(|e| format!("{path}: {e}"))?;
            let mut out = std::io::BufWriter::new(file);
            write_chunked(&mut out, &data, chunk_size)?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// The block index the ndarray at `path` reads from.
fn block_of_ndarray(reader: &Reader, path: &str) -> Result<usize, String> {
    use asdf_core::core::ndarray::{Ndarray, Source};

    let doc = reader
        .tree()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the file has no tree to look in".to_string())?;
    let node = doc.lookup_str(path).ok_or_else(|| format!("no value at {path:?}"))?;
    let array = Ndarray::parse(&doc, node).map_err(|_| format!("{path:?} is not an ndarray"))?;

    match array.source {
        Source::Block(index) => Ok(index),
        Source::LastBlock => {
            reader.block_count().checked_sub(1).ok_or_else(|| "the file has no blocks".to_string())
        }
        Source::External(uri) => Err(format!("{path:?} reads from another file: {uri}")),
        Source::Inline(_) => Err(format!("{path:?} has inline data, not a block")),
    }
}

/// Write `data`, in chunks when a size was asked for.
///
/// The chunking is what upstream's `--chunk-size` controls; it changes how
/// the bytes reach the sink, never which bytes they are.
fn write_chunked(out: &mut impl Write, data: &[u8], chunk_size: usize) -> Result<(), String> {
    if chunk_size == 0 {
        out.write_all(data).map_err(|e| e.to_string())?;
    } else {
        for chunk in data.chunks(chunk_size) {
            out.write_all(chunk).map_err(|e| e.to_string())?;
        }
    }
    out.flush().map_err(|e| e.to_string())
}

/// `asdf events`: the low-level parser event stream, as upstream prints it.
fn cmd_events(args: &[String]) -> Result<ExitCode, String> {
    let mut filename = None;
    let mut verbose = false;
    // Upstream emits YAML events by default and takes `--no-yaml` to stop.
    let mut yaml = true;
    let mut buffer_tree = false;

    for arg in args {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{EVENTS_USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            "-v" | "--verbose" => verbose = true,
            "--no-yaml" => yaml = false,
            "--cap-tree" => buffer_tree = true,
            other if other.starts_with('-') && other.len() > 1 => {
                return Err(format!("unrecognised option {other:?}"));
            }
            other => filename = Some(other.to_string()),
        }
    }

    let filename = filename.ok_or("events requires a FILENAME")?;
    let buf = std::fs::read(&filename).map_err(|e| format!("{filename}: {e}"))?;
    let stream =
        events(&buf, EventOptions { yaml, buffer_tree }).map_err(|e| format!("{filename}: {e}"))?;

    let mut out = String::new();
    for event in &stream {
        out.push_str(&render_event(event, verbose));
    }
    print!("{out}");
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
