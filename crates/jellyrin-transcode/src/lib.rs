use std::{io, process::Stdio};

use jellyrin_core::{FfmpegCommandSpec, FfmpegProgress, parse_ffmpeg_progress_line};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    sync::broadcast,
    task::JoinHandle,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeProcessExit {
    pub code: Option<i32>,
    pub success: bool,
}

pub struct TranscodeProcess {
    child: Option<Child>,
    progress_tx: broadcast::Sender<FfmpegProgress>,
    stderr_task: Option<JoinHandle<io::Result<()>>>,
    exit: Option<TranscodeProcessExit>,
}

pub fn spawn_transcode_process(command: &FfmpegCommandSpec) -> io::Result<TranscodeProcess> {
    let mut child = Command::new(&command.program)
        .args(&command.args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("transcode process stderr was not captured"))?;
    let (progress_tx, _) = broadcast::channel(64);
    let stderr_task = tokio::spawn(read_ffmpeg_progress(stderr, progress_tx.clone()));

    Ok(TranscodeProcess {
        child: Some(child),
        progress_tx,
        stderr_task: Some(stderr_task),
        exit: None,
    })
}

impl TranscodeProcess {
    pub fn process_id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }

    pub fn subscribe_progress(&self) -> broadcast::Receiver<FfmpegProgress> {
        self.progress_tx.subscribe()
    }

    pub async fn wait(&mut self) -> io::Result<TranscodeProcessExit> {
        if let Some(exit) = self.exit.clone() {
            return Ok(exit);
        }

        let mut child = self
            .child
            .take()
            .ok_or_else(|| io::Error::other("transcode process handle is missing"))?;
        let status = child.wait().await?;
        self.finish_stderr_reader().await?;
        let exit = TranscodeProcessExit {
            code: status.code(),
            success: status.success(),
        };
        self.exit = Some(exit.clone());
        Ok(exit)
    }

    pub async fn stop(&mut self) -> io::Result<TranscodeProcessExit> {
        if let Some(exit) = self.exit.clone() {
            return Ok(exit);
        }

        if let Some(child) = self.child.as_mut()
            && child.try_wait()?.is_none()
        {
            child.start_kill()?;
        }

        self.wait().await
    }

    async fn finish_stderr_reader(&mut self) -> io::Result<()> {
        if let Some(stderr_task) = self.stderr_task.take() {
            stderr_task.await.map_err(|error| {
                io::Error::other(format!("stderr reader task failed: {error}"))
            })??;
        }
        Ok(())
    }
}

async fn read_ffmpeg_progress<R>(
    reader: R,
    progress_tx: broadcast::Sender<FfmpegProgress>,
) -> io::Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut progress = FfmpegProgress::default();
    while let Some(line) = lines.next_line().await? {
        if parse_ffmpeg_progress_line_has_snapshot(&mut progress, &line) {
            let _ = progress_tx.send(progress.clone());
        }
    }
    Ok(())
}

fn parse_ffmpeg_progress_line_has_snapshot(progress: &mut FfmpegProgress, line: &str) -> bool {
    let Some((key, _)) = line.trim().split_once('=') else {
        return false;
    };
    let key = key.trim();
    let known = matches!(
        key,
        "frame"
            | "fps"
            | "bitrate"
            | "total_size"
            | "out_time_us"
            | "out_time_ms"
            | "out_time"
            | "speed"
            | "progress"
    );
    if known {
        parse_ffmpeg_progress_line(progress, line);
    }
    key == "progress"
}

#[cfg(test)]
mod tests {
    use jellyrin_core::FfmpegCommandSpec;
    use tokio::time::{Duration, timeout};

    use super::spawn_transcode_process;

    #[tokio::test]
    async fn transcode_process_streams_progress_and_waits_for_exit() {
        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                "printf 'out_time_us=1000\\nprogress=continue\\nout_time_us=2000\\nprogress=end\\n' >&2"
                    .to_string(),
            ],
        );

        let mut process = spawn_transcode_process(&command).unwrap();
        let mut progress = process.subscribe_progress();
        assert!(process.process_id().is_some());

        let first = timeout(Duration::from_secs(5), progress.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.position_ticks(), Some(10000));
        assert_eq!(first.progress.as_deref(), Some("continue"));

        let second = timeout(Duration::from_secs(5), progress.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second.position_ticks(), Some(20000));
        assert!(second.is_complete());

        let exit = process.wait().await.unwrap();
        assert!(exit.success);
        assert_eq!(exit, process.wait().await.unwrap());
    }

    #[tokio::test]
    async fn transcode_process_drains_stderr_without_progress_subscriber() {
        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                "i=0; while [ $i -lt 200 ]; do printf 'out_time_us=%s\\nprogress=continue\\n' \"$i\" >&2; i=$((i + 1)); done"
                    .to_string(),
            ],
        );

        let mut process = spawn_transcode_process(&command).unwrap();
        let exit = timeout(Duration::from_secs(5), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(exit.success);
    }

    #[tokio::test]
    async fn transcode_process_stop_is_idempotent() {
        let command = FfmpegCommandSpec::new("sleep", vec!["30".to_string()]);
        let mut process = spawn_transcode_process(&command).unwrap();
        assert!(process.process_id().is_some());

        let exit = timeout(Duration::from_secs(5), process.stop())
            .await
            .unwrap()
            .unwrap();
        assert!(!exit.success);
        assert_eq!(exit, process.stop().await.unwrap());
    }

    #[tokio::test]
    async fn transcode_process_spawn_failure_is_reported() {
        let command = FfmpegCommandSpec::new("definitely-not-a-jellyrin-command", Vec::new());

        assert!(spawn_transcode_process(&command).is_err());
    }
}
