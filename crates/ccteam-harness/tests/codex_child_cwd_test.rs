#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::time::Duration;

use ccteam_harness::execution::codex_jsonrpc::CodexJsonRpcClient;

struct RestoreCwd(PathBuf);

impl Drop for RestoreCwd {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.0);
    }
}

#[tokio::test]
async fn stdio_app_server_does_not_inherit_a_deleted_daemon_cwd() {
    let root = tempfile::tempdir().unwrap();
    let launch_dir = root.path().join("deploy-worktree");
    std::fs::create_dir(&launch_dir).unwrap();

    let marker = root.path().join("child-cwd");
    let fake_codex = root.path().join("codex");
    std::fs::write(
        &fake_codex,
        format!(
            "#!/bin/sh\npwd -P > '{}'\ncat >/dev/null\n",
            marker.display()
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&fake_codex).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&fake_codex, permissions).unwrap();

    let original_cwd = std::env::current_dir().unwrap();
    std::env::set_current_dir(&launch_dir).unwrap();
    let _restore = RestoreCwd(original_cwd);
    std::fs::remove_dir(&launch_dir).unwrap();

    let client = CodexJsonRpcClient::connect_stdio_command(fake_codex.to_str().unwrap())
        .await
        .expect("app-server must start after its launch worktree is removed");

    tokio::time::timeout(Duration::from_secs(2), async {
        while std::fs::read_to_string(&marker)
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("fake app-server did not record its cwd");

    let child_cwd = std::fs::read_to_string(marker).unwrap();
    assert_eq!(
        PathBuf::from(child_cwd.trim()),
        dirs::home_dir().expect("test user must have a home directory")
    );
    client.terminate_stdio_child().await.unwrap();
}
