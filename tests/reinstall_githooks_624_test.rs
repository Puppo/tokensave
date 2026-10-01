//! #624: `tokensave reinstall` must refresh tokensave's section of git hooks
//! that are already installed, and must not install hooks anywhere they are
//! not.
//!
//! The hook migrations (#342 Q1 fenced post-checkout block, the 7.13.0
//! `--git-common-dir` chain preamble) only ran from `githooks on` and `init`,
//! so after an upgrade `reinstall` left the hooks on the old shape even though
//! `doctor` names `reinstall` as the fix.
//!
//! These run the binary against a throwaway `HOME`, passed only to the child
//! process, so nothing process-global is touched.

#![cfg(not(windows))]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// A post-checkout file as an older release left it: the user's own line, a
/// v1 fenced tokensave block, and more of the user's content after it.
fn stale_post_checkout() -> String {
    format!(
        "#!/bin/sh\n\
         ./scripts/mine.sh \"$@\"\n\
         # tokensave: auto-init\n\
         if [ \"$1\" = \"{ZERO_SHA}\" ]; then\n\
         \ttokensave init >/dev/null 2>&1 &\n\
         fi\n\
         # tokensave: end auto-init\n\
         ./scripts/after.sh\n"
    )
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run git")
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

fn reinstall(cwd: &Path, home: &Path) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_tokensave"))
        .arg("reinstall")
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env_remove("XDG_CONFIG_HOME")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("run tokensave reinstall");
    assert!(
        output.status.success(),
        "reinstall failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn assert_refreshed(path: &Path) {
    let after = fs::read_to_string(path).expect("read post-checkout");
    assert!(
        after.contains("hook post-checkout"),
        "reinstall must rewrite the stale tokensave block in {}:\n{after}",
        path.display()
    );
    assert!(
        !after.contains(ZERO_SHA),
        "the old body must be replaced, not kept beside the new one:\n{after}"
    );
    assert!(
        after.starts_with("#!/bin/sh\n./scripts/mine.sh \"$@\"\n")
            && after.ends_with("./scripts/after.sh\n"),
        "content outside tokensave's markers must survive:\n{after}"
    );
}

#[test]
fn reinstall_refreshes_existing_global_hooks() {
    let home = tempfile::tempdir().expect("temp home");
    let cwd = tempfile::tempdir().expect("temp cwd");
    let hooks = home.path().join(".config").join("git").join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(
        home.path().join(".gitconfig"),
        format!("[core]\n\thooksPath = {}\n", hooks.display()),
    )
    .unwrap();

    // The 7.12 chain preamble, which resolved the repo hook via `--git-dir`
    // and so missed it from linked worktrees.
    let post_commit = "#!/bin/sh\n\
                       # tokensave: chain-repo-hook\n\
                       repo_hook=\"$(git rev-parse --git-dir 2>/dev/null)/hooks/post-commit\"\n\
                       if [ -x \"$repo_hook\" ] && [ \"$repo_hook\" != \"$0\" ]; then\n\
                       \t\"$repo_hook\" \"$@\"\n\
                       fi\n\
                       \n\
                       # tokensave: auto-sync\n\
                       tokensave sync >/dev/null 2>&1 &\n";
    fs::write(hooks.join("post-commit"), post_commit).unwrap();
    fs::write(hooks.join("post-checkout"), stale_post_checkout()).unwrap();

    reinstall(cwd.path(), home.path());

    let commit_after = fs::read_to_string(hooks.join("post-commit")).unwrap();
    assert!(
        commit_after.contains("git rev-parse --git-common-dir")
            && !commit_after.contains("git rev-parse --git-dir"),
        "reinstall must migrate the chain preamble:\n{commit_after}"
    );
    assert!(commit_after.contains("# tokensave: auto-sync"));
    assert_refreshed(&hooks.join("post-checkout"));
}

#[test]
fn reinstall_refreshes_the_current_repositorys_hooks() {
    let home = tempfile::tempdir().expect("temp home");
    let repo = tempfile::tempdir().expect("temp repo");
    git(repo.path(), home.path(), &["init", "-q"]);
    let hooks = repo.path().join(".git").join("hooks");
    fs::create_dir_all(&hooks).unwrap();
    fs::write(hooks.join("post-checkout"), stale_post_checkout()).unwrap();

    reinstall(repo.path(), home.path());

    assert_refreshed(&hooks.join("post-checkout"));
}

#[test]
fn reinstall_does_not_install_hooks_nobody_opted_into() {
    let home = tempfile::tempdir().expect("temp home");
    let repo = tempfile::tempdir().expect("temp repo");
    git(repo.path(), home.path(), &["init", "-q"]);
    let local_hooks = repo.path().join(".git").join("hooks");
    let global_hooks = home.path().join(".config").join("git").join("hooks");

    reinstall(repo.path(), home.path());

    for name in ["post-commit", "post-checkout", "post-merge"] {
        assert!(
            !local_hooks.join(name).exists(),
            "reinstall must not install a local {name} hook"
        );
        assert!(
            !global_hooks.join(name).exists(),
            "reinstall must not install a global {name} hook"
        );
    }
    let gitconfig = fs::read_to_string(home.path().join(".gitconfig")).unwrap_or_default();
    assert!(
        !gitconfig.contains("hooksPath"),
        "reinstall must not claim core.hooksPath:\n{gitconfig}"
    );
}
