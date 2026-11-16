//! Stand-in for `lake` in the Mathlib setup integration tests. It is never
//! launched by the application itself and never uses the network.
//!
//! Run in a package folder, it does what the test laid out in `fake-lake/`:
//! - `… update`: copies `fake-lake/manifest.json` to `lake-manifest.json` (what
//!   Lake "resolved") and succeeds; fails when the test laid out none.
//! - `exe cache get`: records that the cache program ran (`fake-lake/cache-ran`)
//!   and fails, as a cache step without a cache does.
//! - anything else fails.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let laid_out = Path::new("fake-lake");
    match args.as_slice() {
        [.., "update"] => match std::fs::copy(laid_out.join("manifest.json"), "lake-manifest.json") {
            Ok(_) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: no manifest laid out: {e}");
                ExitCode::FAILURE
            }
        },
        ["exe", "cache", "get"] => {
            let _ = std::fs::write(laid_out.join("cache-ran"), b"");
            eprintln!("error: no cache in the stand-in");
            ExitCode::FAILURE
        }
        _ => {
            eprintln!("error: the stand-in lake does not do this");
            ExitCode::FAILURE
        }
    }
}
