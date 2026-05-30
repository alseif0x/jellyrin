use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use jellyrin_core::{
    DEFAULT_HLS_SEGMENT_PATTERN, FfmpegCommandSpec, FfmpegProgress, parse_ffmpeg_progress_line,
};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncBufReadExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::broadcast,
    task::JoinHandle,
    time::{Instant, sleep},
};

pub const HLS_MASTER_PLAYLIST_NAME: &str = "master.m3u8";
pub const HLS_MEDIA_PLAYLIST_NAME: &str = "main.m3u8";
pub const DEFAULT_HLS_POLL_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscodeProcessExit {
    pub code: Option<i32>,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlsTranscodeLayout {
    pub session_dir: PathBuf,
    pub master_playlist_path: PathBuf,
    pub media_playlist_path: PathBuf,
    pub segment_pattern_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlsVariantInfo {
    pub uri: String,
    pub bandwidth: u32,
    pub resolution: Option<(u32, u32)>,
    pub codecs: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HlsSegment {
    pub duration_seconds: f64,
    pub uri: String,
}

pub struct TranscodeProcess {
    child: Option<Child>,
    progress_tx: broadcast::Sender<FfmpegProgress>,
    stderr_task: Option<JoinHandle<io::Result<()>>>,
    exit: Option<TranscodeProcessExit>,
}

impl HlsTranscodeLayout {
    pub fn new(root: impl AsRef<Path>, play_session_id: &str) -> Self {
        let session_dir = root
            .as_ref()
            .join(sanitize_hls_path_component(play_session_id));
        Self::from_session_dir(session_dir)
    }

    pub fn from_media_playlist_path(media_playlist_path: impl AsRef<Path>) -> Self {
        let media_playlist_path = media_playlist_path.as_ref().to_path_buf();
        let session_dir = media_playlist_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        Self {
            master_playlist_path: session_dir.join(HLS_MASTER_PLAYLIST_NAME),
            segment_pattern_path: session_dir.join(DEFAULT_HLS_SEGMENT_PATTERN),
            media_playlist_path,
            session_dir,
        }
    }

    fn from_session_dir(session_dir: PathBuf) -> Self {
        Self {
            master_playlist_path: session_dir.join(HLS_MASTER_PLAYLIST_NAME),
            media_playlist_path: session_dir.join(HLS_MEDIA_PLAYLIST_NAME),
            segment_pattern_path: session_dir.join(DEFAULT_HLS_SEGMENT_PATTERN),
            session_dir,
        }
    }

    pub fn segment_path(&self, index: u32) -> PathBuf {
        self.session_dir.join(format!("segment_{index:05}.ts"))
    }

    pub fn segment_pattern_string(&self) -> String {
        self.segment_pattern_path.to_string_lossy().to_string()
    }
}

pub fn spawn_transcode_process(command: &FfmpegCommandSpec) -> io::Result<TranscodeProcess> {
    spawn_transcode_process_with_stdin_mode(command, false).map(|(process, _stdin)| process)
}

pub fn spawn_transcode_process_with_stdin(
    command: &FfmpegCommandSpec,
) -> io::Result<(TranscodeProcess, ChildStdin)> {
    let (process, stdin) = spawn_transcode_process_with_stdin_mode(command, true)?;
    let stdin =
        stdin.ok_or_else(|| io::Error::other("transcode process stdin was not captured"))?;
    Ok((process, stdin))
}

fn spawn_transcode_process_with_stdin_mode(
    command: &FfmpegCommandSpec,
    pipe_stdin: bool,
) -> io::Result<(TranscodeProcess, Option<ChildStdin>)> {
    let mut child = Command::new(&command.program)
        .args(&command.args)
        .stdin(if pipe_stdin {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stdin = if pipe_stdin { child.stdin.take() } else { None };
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("transcode process stderr was not captured"))?;
    let (progress_tx, _) = broadcast::channel(64);
    let stderr_task = tokio::spawn(read_ffmpeg_progress(stderr, progress_tx.clone()));

    let process = TranscodeProcess {
        child: Some(child),
        progress_tx,
        stderr_task: Some(stderr_task),
        exit: None,
    };
    Ok((process, stdin))
}

pub fn render_hls_master_playlist(variant: &HlsVariantInfo) -> String {
    let mut attributes = vec![format!("BANDWIDTH={}", variant.bandwidth)];
    if let Some((width, height)) = variant.resolution {
        attributes.push(format!("RESOLUTION={width}x{height}"));
    }
    if let Some(codecs) = variant
        .codecs
        .as_deref()
        .filter(|codecs| !codecs.is_empty())
    {
        attributes.push(format!("CODECS=\"{}\"", escape_hls_attribute(codecs)));
    }

    format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-STREAM-INF:{}\n{}\n",
        attributes.join(","),
        variant.uri
    )
}

pub fn render_hls_media_playlist(
    target_duration_seconds: u32,
    media_sequence: u64,
    segments: &[HlsSegment],
    end_list: bool,
) -> String {
    let mut playlist = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
        target_duration_seconds.max(1),
        media_sequence
    );
    for segment in segments {
        playlist.push_str(&format!(
            "#EXTINF:{:.3},\n{}\n",
            segment.duration_seconds.max(0.0),
            segment.uri
        ));
    }
    if end_list {
        playlist.push_str("#EXT-X-ENDLIST\n");
    }
    playlist
}

pub async fn wait_for_hls_readiness(
    media_playlist_path: impl AsRef<Path>,
    first_segment_path: impl AsRef<Path>,
    timeout: Duration,
) -> io::Result<bool> {
    let media_playlist_path = media_playlist_path.as_ref();
    let first_segment_path = first_segment_path.as_ref();
    let deadline = Instant::now() + timeout;

    loop {
        if non_empty_file_exists(media_playlist_path).await?
            && non_empty_file_exists(first_segment_path).await?
        {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        sleep(DEFAULT_HLS_POLL_INTERVAL).await;
    }
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

fn sanitize_hls_path_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|character| match character {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' => character,
            _ => '_',
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "unknown".to_string()
    } else {
        sanitized
    }
}

fn escape_hls_attribute(value: &str) -> String {
    value.replace('"', "\\\"")
}

async fn non_empty_file_exists(path: &Path) -> io::Result<bool> {
    match fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_file() && metadata.len() > 0),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
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
    use tokio::io::AsyncWriteExt;
    use tokio::time::{Duration, timeout};

    use super::{
        HLS_MASTER_PLAYLIST_NAME, HLS_MEDIA_PLAYLIST_NAME, HlsSegment, HlsTranscodeLayout,
        HlsVariantInfo, render_hls_master_playlist, render_hls_media_playlist,
        spawn_transcode_process, spawn_transcode_process_with_stdin, wait_for_hls_readiness,
    };

    #[test]
    fn hls_layout_uses_sanitized_session_directory_and_expected_names() {
        let root = tempfile::tempdir().unwrap();
        let layout = HlsTranscodeLayout::new(root.path(), "../play session:1");

        assert_eq!(layout.session_dir, root.path().join("___play_session_1"));
        assert_eq!(
            layout.master_playlist_path,
            layout.session_dir.join(HLS_MASTER_PLAYLIST_NAME)
        );
        assert_eq!(
            layout.media_playlist_path,
            layout.session_dir.join(HLS_MEDIA_PLAYLIST_NAME)
        );
        assert_eq!(
            layout.segment_pattern_path,
            layout.session_dir.join("segment_%05d.ts")
        );
        assert_eq!(
            layout.segment_path(7),
            layout.session_dir.join("segment_00007.ts")
        );
    }

    #[test]
    fn hls_layout_can_be_derived_from_persisted_output_path() {
        let root = tempfile::tempdir().unwrap();
        let output_path = root.path().join("play-1").join(HLS_MEDIA_PLAYLIST_NAME);
        let layout = HlsTranscodeLayout::from_media_playlist_path(&output_path);

        assert_eq!(layout.session_dir, root.path().join("play-1"));
        assert_eq!(layout.media_playlist_path, output_path);
        assert_eq!(
            layout.master_playlist_path,
            root.path().join("play-1").join(HLS_MASTER_PLAYLIST_NAME)
        );
        assert_eq!(
            layout.segment_pattern_path,
            root.path().join("play-1").join("segment_%05d.ts")
        );
    }

    #[test]
    fn renders_hls_master_playlist_snapshot() {
        let playlist = render_hls_master_playlist(&HlsVariantInfo {
            uri: HLS_MEDIA_PLAYLIST_NAME.to_string(),
            bandwidth: 4_000_000,
            resolution: Some((1280, 720)),
            codecs: Some("avc1.4d401f,mp4a.40.2".to_string()),
        });

        assert_eq!(
            playlist,
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-STREAM-INF:BANDWIDTH=4000000,RESOLUTION=1280x720,CODECS=\"avc1.4d401f,mp4a.40.2\"\n\
             main.m3u8\n"
        );
    }

    #[test]
    fn renders_hls_media_playlist_snapshot() {
        let playlist = render_hls_media_playlist(
            3,
            0,
            &[
                HlsSegment {
                    duration_seconds: 3.003,
                    uri: "segment_00000.ts".to_string(),
                },
                HlsSegment {
                    duration_seconds: 2.5,
                    uri: "segment_00001.ts".to_string(),
                },
            ],
            true,
        );

        assert_eq!(
            playlist,
            "#EXTM3U\n\
             #EXT-X-VERSION:3\n\
             #EXT-X-TARGETDURATION:3\n\
             #EXT-X-MEDIA-SEQUENCE:0\n\
             #EXTINF:3.003,\n\
             segment_00000.ts\n\
             #EXTINF:2.500,\n\
             segment_00001.ts\n\
             #EXT-X-ENDLIST\n"
        );
    }

    #[tokio::test]
    async fn hls_readiness_waits_for_playlist_and_first_segment() {
        let root = tempfile::tempdir().unwrap();
        let layout = HlsTranscodeLayout::new(root.path(), "play-1");
        tokio::fs::create_dir_all(&layout.session_dir)
            .await
            .unwrap();
        let media_playlist_path = layout.media_playlist_path.clone();
        let first_segment_path = layout.segment_path(0);

        let write_playlist = media_playlist_path.clone();
        let write_segment = first_segment_path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            tokio::fs::write(write_playlist, b"#EXTM3U\n")
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(50)).await;
            tokio::fs::write(write_segment, b"ts").await.unwrap();
        });

        assert!(
            wait_for_hls_readiness(
                &media_playlist_path,
                &first_segment_path,
                Duration::from_secs(5)
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn hls_readiness_times_out_without_first_segment() {
        let root = tempfile::tempdir().unwrap();
        let layout = HlsTranscodeLayout::new(root.path(), "play-1");
        tokio::fs::create_dir_all(&layout.session_dir)
            .await
            .unwrap();
        tokio::fs::write(&layout.media_playlist_path, b"#EXTM3U\n")
            .await
            .unwrap();

        assert!(
            !wait_for_hls_readiness(
                &layout.media_playlist_path,
                layout.segment_path(0),
                Duration::from_millis(100)
            )
            .await
            .unwrap()
        );
    }

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
    async fn transcode_process_with_stdin_forwards_pipe_bytes() {
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("stdin.out");
        let command = FfmpegCommandSpec::new(
            "sh",
            vec![
                "-c".to_string(),
                format!("cat > {}", output.to_string_lossy()),
            ],
        );

        let (mut process, mut stdin) = spawn_transcode_process_with_stdin(&command).unwrap();
        stdin.write_all(b"pipe-bytes").await.unwrap();
        stdin.shutdown().await.unwrap();
        drop(stdin);

        let exit = timeout(Duration::from_secs(5), process.wait())
            .await
            .unwrap()
            .unwrap();
        assert!(exit.success);
        assert_eq!(tokio::fs::read(output).await.unwrap(), b"pipe-bytes");
    }

    #[tokio::test]
    async fn transcode_process_spawn_failure_is_reported() {
        let command = FfmpegCommandSpec::new("definitely-not-a-jellyrin-command", Vec::new());

        assert!(spawn_transcode_process(&command).is_err());
    }
}
