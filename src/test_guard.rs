//! Tests must never start the test binary again. A unit test that ends
//! up exec'ing it (as a tab's "claude", a brain's MCP server, a doctor
//! tab) would run the whole suite again, which starts it again: a fork
//! bomb. Two guards:
//!
//! - Before main, the test binary marks the environment with its pid
//!   (GODTERM_TEST_PROCESS). A test binary that starts and finds the mark
//!   already set from another process was started by a test: it exits at
//!   once (99), before libtest runs anything.
//! - Code that launches programs refuses, in tests, a program that is
//!   this very binary (by path or by hard link), see `is_current_exe`.

/// Set in the environment of the test process and everything it starts.
pub const ENV: &str = "GODTERM_TEST_PROCESS";

/// Runs before main (and before libtest) in the unit test binary.
#[cfg(test)]
extern "C" fn guard() {
    let me = std::process::id().to_string();
    match std::env::var(ENV) {
        // Started by another test process: never run the suite again.
        // (The Windows Ctrl-C helper re-runs one exact test on purpose.)
        Ok(p) if p != me && std::env::var_os("GODTERM_CTRL_C_PID").is_none() => {
            eprintln!("godterm test binary started by a test (pid {p}); refusing to run");
            std::process::exit(99);
        }
        _ => std::env::set_var(ENV, &me),
    }
}

#[cfg(test)]
#[used]
#[cfg_attr(target_os = "macos", link_section = "__DATA,__mod_init_func")]
#[cfg_attr(target_os = "linux", link_section = ".init_array")]
#[cfg_attr(windows, link_section = ".CRT$XCU")]
static GUARD: extern "C" fn() = guard;

/// `program` is the running binary itself (same file, or a hard link to it).
pub fn is_current_exe(program: &std::path::Path) -> bool {
    let Ok(me) = std::env::current_exe() else {
        return false;
    };
    let resolve = |p: &std::path::Path| -> Option<std::path::PathBuf> {
        if p.components().count() > 1 {
            p.canonicalize().ok()
        } else {
            // A bare name: where PATH finds it.
            crate::voice::tts::which(&p.to_string_lossy()).and_then(|x| x.canonicalize().ok())
        }
    };
    let (Some(a), Some(b)) = (resolve(program), me.canonicalize().ok()) else {
        return false;
    };
    if a == b {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(x), Ok(y)) = (std::fs::metadata(&a), std::fs::metadata(&b)) {
            return x.dev() == y.dev() && x.ino() == y.ino();
        }
    }
    false
}

/// Whether this binary looks like a cargo test harness
/// (`target/<profile>/deps/<crate>-<16 hex>`): never something to hand
/// out as an agent.
pub fn looks_like_test_harness(exe: &std::path::Path) -> bool {
    let in_deps = exe
        .parent()
        .and_then(|p| p.file_name())
        .is_some_and(|n| n == "deps");
    let stem = exe
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let hashed = stem
        .rsplit_once('-')
        .is_some_and(|(_, h)| h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit()));
    in_deps && hashed
}

#[cfg(test)]
mod tests {
    #[test]
    fn this_process_is_marked_and_knows_itself() {
        // The guard ran before main and marked us with our own pid.
        assert_eq!(
            std::env::var(super::ENV).ok(),
            Some(std::process::id().to_string())
        );
        let me = std::env::current_exe().unwrap();
        assert!(super::is_current_exe(&me));
        assert!(super::looks_like_test_harness(&me), "{}", me.display());
        assert!(!super::is_current_exe(std::path::Path::new(
            &crate::test_stub::true_bin()
        )));
        assert!(!super::looks_like_test_harness(std::path::Path::new(
            "/Users/x/claudego/target/release/godterm"
        )));
    }
}
