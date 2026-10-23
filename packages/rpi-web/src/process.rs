//! Background children must not allocate a console when Web has no console.
use std::ffi::OsStr;
use tokio::process::Command;

pub(crate) fn background_command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    command.kill_on_drop(true);
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW: redirecting stdio alone does not prevent Windows
        // from opening a console for git.exe or the native RPC host.
        command.creation_flags(0x0800_0000);
    }
    command
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::{process::Stdio, time::Duration};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn background_child_has_no_console_and_preserves_piped_stdio() {
        let script = r#"
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class ConsoleProbe { [DllImport("kernel32.dll")] public static extern IntPtr GetConsoleWindow(); }'
[Console]::WriteLine([ConsoleProbe]::GetConsoleWindow().ToInt64())
[Console]::WriteLine([Console]::ReadLine())
[Console]::Error.WriteLine('background stderr')
"#;
        let mut child = background_command("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start Windows console probe");
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(b"background stdin\n").await.unwrap();
        drop(stdin);
        let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
            .await
            .expect("console probe timed out")
            .expect("read console probe output");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(
            String::from_utf8(output.stdout)
                .unwrap()
                .replace("\r\n", "\n"),
            "0\nbackground stdin\n"
        );
        assert_eq!(
            String::from_utf8(output.stderr).unwrap().trim(),
            "background stderr"
        );
    }
}
