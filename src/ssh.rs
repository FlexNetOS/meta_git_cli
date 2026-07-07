use console::style;
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

/// Discover unique SSH remote URLs from the .meta config in the current directory.
/// Returns the full SSH URLs (preserving user, host, and port) so callers can
/// pass them directly to `establish_ssh_masters` without information loss.
/// Returns an empty list if no .meta config is found or no SSH URLs exist.
pub fn discover_ssh_urls(cwd: &Path) -> Vec<String> {
    let mut urls: BTreeSet<String> = discover_actual_ssh_urls(cwd).into_iter().collect();

    let Some((config_path, _format)) = meta_core::config::find_meta_config(cwd, None) else {
        return urls.into_iter().collect();
    };

    if let Ok((projects, _ignore)) = meta_core::config::parse_meta_config(&config_path) {
        for project in &projects {
            if let Some(repo) = &project.repo {
                if meta_git_lib::extract_ssh_host(repo).is_some() {
                    urls.insert(repo.clone());
                }
            }

            let repo_path = cwd.join(&project.path);
            for remote in git_origin_urls(&repo_path) {
                if meta_git_lib::extract_ssh_host(&remote).is_some() {
                    urls.insert(remote);
                }
            }
        }
    }

    urls.into_iter().collect()
}

fn discover_actual_ssh_urls(cwd: &Path) -> Vec<String> {
    git_origin_urls(cwd)
        .into_iter()
        .filter(|url| meta_git_lib::extract_ssh_host(url).is_some())
        .collect()
}

fn git_origin_urls(repo_path: &Path) -> Vec<String> {
    let mut urls = BTreeSet::new();

    for args in [
        ["remote", "get-url", "origin"].as_slice(),
        ["remote", "get-url", "--push", "origin"].as_slice(),
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo_path)
            .output();

        let Ok(output) = output else {
            continue;
        };

        if !output.status.success() {
            continue;
        }

        for line in String::from_utf8_lossy(&output.stdout).lines() {
            let url = line.trim();
            if !url.is_empty() {
                urls.insert(url.to_string());
            }
        }
    }

    urls.into_iter().collect()
}

/// A remote URL mismatch between .meta config and the actual repo.
pub(crate) struct RemoteMismatch {
    pub name: String,
    /// The configured path (may differ from name for custom paths / nested repos).
    pub path: String,
    pub expected: String,
    pub actual: String,
}

/// Check child repos for remote URL mismatches against .meta config.
/// Returns a list of mismatches found (non-interactive, no prompts).
pub(crate) fn find_remote_mismatches(cwd: &Path) -> Vec<RemoteMismatch> {
    let Some((config_path, _format)) = meta_core::config::find_meta_config(cwd, None) else {
        return vec![];
    };

    let Ok((projects, _ignore)) = meta_core::config::parse_meta_config(&config_path) else {
        return vec![];
    };

    let mut mismatches = Vec::new();

    for project in &projects {
        let Some(expected_url) = &project.repo else {
            continue;
        };

        let repo_path = cwd.join(&project.path);
        if !repo_path.join(".git").exists() && !repo_path.exists() {
            continue;
        }

        let Some(actual_url) = meta_git_lib::get_remote_url(&repo_path) else {
            continue;
        };

        if !meta_git_lib::urls_match(&actual_url, expected_url) {
            mismatches.push(RemoteMismatch {
                name: project.name.clone(),
                path: project.path.clone(),
                expected: expected_url.clone(),
                actual: actual_url,
            });
        }
    }

    mismatches
}

/// Print warnings about remote URL mismatches (non-interactive).
pub(crate) fn warn_remote_mismatches(cwd: &Path) {
    let mismatches = find_remote_mismatches(cwd);

    if mismatches.is_empty() {
        return;
    }

    eprintln!(
        "{} Found {} remote URL mismatch{}:",
        style("⚠").yellow().bold(),
        mismatches.len(),
        if mismatches.len() == 1 { "" } else { "es" }
    );

    for m in &mismatches {
        eprintln!("  {}", style(&m.name).bold());
        eprintln!("    actual:   {}", style(&m.actual).red());
        eprintln!("    expected: {}", style(&m.expected).green());
    }

    eprintln!();
    eprintln!("  Fix manually with:");
    for m in &mismatches {
        eprintln!(
            "    git -C '{}' remote set-url origin '{}'",
            m.path, m.expected
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn init_repo(path: &Path, fetch_url: &str, push_url: Option<&str>) {
        fs::create_dir_all(path).unwrap();
        let status = Command::new("git")
            .args(["init", "-q"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());

        let status = Command::new("git")
            .args(["remote", "add", "origin", fetch_url])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());

        if let Some(push_url) = push_url {
            let status = Command::new("git")
                .args(["remote", "set-url", "--push", "origin", push_url])
                .current_dir(path)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    #[test]
    fn discover_ssh_urls_includes_actual_push_remote() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(tmp.path(), "git@github.com:gitkb/meta.git", None);
        fs::write(
            tmp.path().join(".meta.yaml"),
            r#"
projects:
  codex-plugins:
    repo: git@github.com:gitkb/codex-plugins.git
"#,
        )
        .unwrap();
        init_repo(
            &tmp.path().join("codex-plugins"),
            "https://github.com/gitkb/codex-plugins.git",
            Some("git@github.com:FlexNetOS/codex-plugins.git"),
        );

        let urls = discover_ssh_urls(tmp.path());

        assert!(urls.contains(&"git@github.com:gitkb/meta.git".to_string()));
        assert!(urls.contains(&"git@github.com:gitkb/codex-plugins.git".to_string()));
        assert!(urls.contains(&"git@github.com:FlexNetOS/codex-plugins.git".to_string()));
    }

    #[test]
    fn discover_ssh_urls_without_meta_uses_current_repo_remotes() {
        let tmp = tempfile::tempdir().unwrap();
        init_repo(
            tmp.path(),
            "https://github.com/gitkb/meta.git",
            Some("git@github.com:FlexNetOS/meta.git"),
        );

        let urls = discover_ssh_urls(tmp.path());

        assert_eq!(urls, vec!["git@github.com:FlexNetOS/meta.git".to_string()]);
    }
}
