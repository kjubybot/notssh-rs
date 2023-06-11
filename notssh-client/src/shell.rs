use std::process::{Output, Stdio};

use notssh_util::error;
use tokio::{io::AsyncWriteExt, process::Command};

pub struct Runner {
    cmd: Command,
    stdin: Vec<u8>,
}

impl Runner {
    pub fn new(cmd: String, args: Vec<String>, stdin: Vec<u8>) -> Self {
        let mut cmd = Command::new(cmd);
        cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        if !stdin.is_empty() {
            cmd.stdin(Stdio::piped());
        }
        Self { cmd, stdin }
    }

    pub async fn run(mut self) -> error::Result<Output> {
        let mut child = self.cmd.spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(&self.stdin).await?;
        }
        child.wait_with_output().await.map_err(From::from)
    }
}
