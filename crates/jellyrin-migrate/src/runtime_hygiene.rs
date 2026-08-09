use std::{
    fs::{File, OpenOptions},
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Instant,
};

use serde::Serialize;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use zeroize::Zeroize;

const READ_CHUNK_BYTES: usize = 16 * 1024;
const MAX_LOG_UNIT_BYTES: usize = 1024 * 1024;
const MAX_ARGV_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default)]
pub struct RuntimeHygieneAuditOptions {
    pub log_files: Vec<PathBuf>,
    pub argv_files: Vec<PathBuf>,
    pub relay_port: Option<u16>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RuntimeHygieneCounts {
    pub url_userinfo: u64,
    pub sensitive_query: u64,
    pub xtream_credential_path: u64,
    pub upstream_media_argv: u64,
    pub oversized_input_units: u64,
}

impl RuntimeHygieneCounts {
    pub fn has_findings(&self) -> bool {
        self.url_userinfo != 0
            || self.sensitive_query != 0
            || self.xtream_credential_path != 0
            || self.upstream_media_argv != 0
    }

    fn merge(&mut self, other: &Self) {
        self.url_userinfo += other.url_userinfo;
        self.sensitive_query += other.sensitive_query;
        self.xtream_credential_path += other.xtream_credential_path;
        self.upstream_media_argv += other.upstream_media_argv;
        self.oversized_input_units += other.oversized_input_units;
    }
}

#[derive(Debug, Serialize)]
pub struct RuntimeHygieneReport {
    pub report_version: u32,
    pub tool_version: &'static str,
    pub status: &'static str,
    pub sources_requested: u64,
    pub sources_scanned: u64,
    pub incomplete_sources: u64,
    pub counts: RuntimeHygieneCounts,
    pub started_at: String,
    pub finished_at: String,
    pub duration_ms: u128,
}

impl RuntimeHygieneReport {
    pub fn exit_code(&self) -> i32 {
        if self.incomplete_sources != 0 || self.counts.oversized_input_units != 0 {
            3
        } else if self.counts.has_findings() {
            2
        } else {
            0
        }
    }
}

/// Scans explicitly supplied logs and `/proc/*/cmdline` snapshots without retaining or returning
/// their contents. Every source is opened with `O_NOFOLLOW`; unreadable, non-regular, changing or
/// oversized input makes the result incomplete instead of silently passing it.
pub fn audit_runtime_hygiene(
    options: RuntimeHygieneAuditOptions,
) -> anyhow::Result<RuntimeHygieneReport> {
    let started = OffsetDateTime::now_utc();
    let timer = Instant::now();
    let sources_requested = options.log_files.len() + options.argv_files.len();
    let mut sources_scanned = 0_u64;
    let mut incomplete_sources = u64::from(
        sources_requested == 0 || (!options.argv_files.is_empty() && options.relay_port.is_none()),
    );
    let mut counts = RuntimeHygieneCounts::default();

    for path in options.log_files {
        match open_regular_nofollow(&path).and_then(scan_log) {
            Ok(scanned) => {
                sources_scanned += 1;
                counts.merge(&scanned);
            }
            Err(_) => incomplete_sources += 1,
        }
    }
    for path in options.argv_files {
        match open_regular_nofollow(&path)
            .and_then(|file| scan_argv(file, options.relay_port.unwrap_or_default()))
        {
            Ok(scanned) => {
                sources_scanned += 1;
                counts.merge(&scanned);
            }
            Err(_) => incomplete_sources += 1,
        }
    }

    let finished = OffsetDateTime::now_utc();
    let status = if incomplete_sources != 0 || counts.oversized_input_units != 0 {
        "runtime_hygiene_incomplete"
    } else if counts.has_findings() {
        "runtime_hygiene_findings"
    } else {
        "runtime_hygiene_clean"
    };
    Ok(RuntimeHygieneReport {
        report_version: 1,
        tool_version: env!("CARGO_PKG_VERSION"),
        status,
        sources_requested: u64::try_from(sources_requested)?,
        sources_scanned,
        incomplete_sources,
        counts,
        started_at: started.format(&Rfc3339)?,
        finished_at: finished.format(&Rfc3339)?,
        duration_ms: timer.elapsed().as_millis(),
    })
}

fn open_regular_nofollow(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "audit input is not a regular file",
        ));
    }
    Ok(file)
}

fn scan_log(mut file: File) -> io::Result<RuntimeHygieneCounts> {
    let before = FileIdentity::from_metadata(&file.metadata()?);
    let mut counts = RuntimeHygieneCounts::default();
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    let mut unit = Vec::with_capacity(4096);
    let mut oversized = false;
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        for &byte in &chunk[..read] {
            if is_log_delimiter(byte) {
                finish_log_unit(&mut unit, &mut oversized, &mut counts);
            } else if unit.len() < MAX_LOG_UNIT_BYTES {
                unit.push(byte);
            } else {
                oversized = true;
            }
        }
        chunk[..read].zeroize();
    }
    finish_log_unit(&mut unit, &mut oversized, &mut counts);
    if before != FileIdentity::from_metadata(&file.metadata()?) {
        return Err(io::Error::other("audit input changed while it was read"));
    }
    Ok(counts)
}

fn finish_log_unit(unit: &mut Vec<u8>, oversized: &mut bool, counts: &mut RuntimeHygieneCounts) {
    if *oversized {
        counts.oversized_input_units += 1;
    } else if !unit.is_empty() {
        scan_unit(unit, None, counts);
    }
    unit.zeroize();
    unit.clear();
    *oversized = false;
}

fn scan_argv(mut file: File, relay_port: u16) -> io::Result<RuntimeHygieneCounts> {
    let before = FileIdentity::from_metadata(&file.metadata()?);
    let mut bytes = Vec::new();
    let read = file
        .by_ref()
        .take(MAX_ARGV_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(read).unwrap_or(u64::MAX) > MAX_ARGV_BYTES {
        bytes.zeroize();
        return Ok(RuntimeHygieneCounts {
            oversized_input_units: 1,
            ..RuntimeHygieneCounts::default()
        });
    }
    if bytes.is_empty() || bytes.last() != Some(&0) {
        bytes.zeroize();
        return Ok(RuntimeHygieneCounts {
            oversized_input_units: 1,
            ..RuntimeHygieneCounts::default()
        });
    }
    let args = bytes
        .split(|byte| *byte == 0)
        .filter(|arg| !arg.is_empty())
        .collect::<Vec<_>>();
    let media_process = args.first().is_some_and(|arg| is_media_executable(arg));
    let mut counts = RuntimeHygieneCounts::default();
    for arg in args {
        scan_unit(arg, media_process.then_some(relay_port), &mut counts);
    }
    bytes.zeroize();
    if before != FileIdentity::from_metadata(&file.metadata()?) {
        return Err(io::Error::other("audit input changed while it was read"));
    }
    Ok(counts)
}

#[derive(Debug, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    length: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl FileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            length: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        }
    }
}

fn scan_unit(unit: &[u8], media_relay_port: Option<u16>, counts: &mut RuntimeHygieneCounts) {
    counts.xtream_credential_path += count_xtream_paths(unit);
    let mut cursor = 0;
    while let Some((start, scheme_len)) = find_http_scheme(&unit[cursor..]) {
        let start = cursor + start;
        let end = unit[start..]
            .iter()
            .position(|byte| is_url_delimiter(*byte))
            .map_or(unit.len(), |length| start + length);
        if end <= start + scheme_len {
            cursor = start + scheme_len;
            continue;
        }
        let candidate = &unit[start..end];
        if has_raw_userinfo(candidate, scheme_len) {
            counts.url_userinfo += 1;
        }
        if has_raw_sensitive_query(candidate) {
            counts.sensitive_query += 1;
        }
        if media_relay_port.is_some()
            && !(start == 0
                && end == unit.len()
                && is_internal_media_relay(candidate, media_relay_port.unwrap_or_default()))
        {
            counts.upstream_media_argv += 1;
        }
        cursor = end.max(start + scheme_len);
    }
}

fn is_log_delimiter(byte: u8) -> bool {
    byte.is_ascii_whitespace() || byte == 0 || matches!(byte, b'"' | b'\'' | b'<' | b'>')
}

fn is_url_delimiter(byte: u8) -> bool {
    is_log_delimiter(byte) || matches!(byte, b'{' | b'}')
}

fn find_http_scheme(bytes: &[u8]) -> Option<(usize, usize)> {
    (0..bytes.len()).find_map(|index| {
        let rest = &bytes[index..];
        if starts_ascii_case_insensitive(rest, b"https://") {
            Some((index, 8))
        } else if starts_ascii_case_insensitive(rest, b"http://") {
            Some((index, 7))
        } else {
            None
        }
    })
}

fn has_raw_userinfo(candidate: &[u8], scheme_len: usize) -> bool {
    let authority = &candidate[scheme_len
        ..candidate[scheme_len..]
            .iter()
            .position(|byte| matches!(byte, b'/' | b'?' | b'#'))
            .map_or(candidate.len(), |position| scheme_len + position)];
    authority
        .iter()
        .position(|byte| *byte == b'@')
        .is_some_and(|at| authority[..at].contains(&b':') || !authority[..at].is_empty())
}

fn has_raw_sensitive_query(candidate: &[u8]) -> bool {
    let Some(query_start) = candidate.iter().position(|byte| *byte == b'?') else {
        return false;
    };
    candidate[query_start + 1..]
        .split(|byte| matches!(byte, b'&' | b';' | b'#'))
        .filter_map(|pair| pair.split(|byte| *byte == b'=').next())
        .any(is_sensitive_query_key)
}

fn is_sensitive_query_key(key: &[u8]) -> bool {
    const KEYS: [&[u8]; 11] = [
        b"username",
        b"password",
        b"token",
        b"access_token",
        b"api_key",
        b"apikey",
        b"authorization",
        b"cookie",
        b"secret",
        b"signature",
        b"sig",
    ];
    let mut decoded = Vec::with_capacity(key.len().min(64));
    let mut cursor = 0;
    while cursor < key.len() {
        match key[cursor] {
            b'%' if cursor + 2 < key.len() => {
                let Some(high) = hex_value(key[cursor + 1]) else {
                    return false;
                };
                let Some(low) = hex_value(key[cursor + 2]) else {
                    return false;
                };
                decoded.push((high << 4) | low);
                cursor += 3;
            }
            b'+' => {
                decoded.push(b' ');
                cursor += 1;
            }
            b'-' => {
                decoded.push(b'_');
                cursor += 1;
            }
            byte => {
                decoded.push(byte);
                cursor += 1;
            }
        }
        if decoded.len() > 64 {
            return false;
        }
    }
    KEYS.iter()
        .any(|expected| decoded.eq_ignore_ascii_case(expected))
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn count_xtream_paths(bytes: &[u8]) -> u64 {
    const PREFIXES: [&[u8]; 4] = [b"/live/", b"/movie/", b"/series/", b"/timeshift/"];
    let mut count = 0;
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some((offset, prefix_len)) = PREFIXES
            .iter()
            .filter_map(|prefix| {
                find_ascii_case_insensitive(&bytes[cursor..], prefix)
                    .map(|offset| (offset, prefix.len()))
            })
            .min_by_key(|(offset, _)| *offset)
        else {
            break;
        };
        let start = cursor + offset + prefix_len;
        let first_end = bytes[start..].iter().position(|byte| *byte == b'/');
        let matched = first_end.is_some_and(|first_len| {
            first_len != 0
                && bytes[start + first_len + 1..]
                    .iter()
                    .position(|byte| matches!(byte, b'/' | b'?' | b'#'))
                    .is_some_and(|second_len| second_len != 0)
        });
        if matched {
            count += 1;
        }
        cursor = start.max(cursor + 1);
    }
    count
}

fn is_media_executable(arg: &[u8]) -> bool {
    let basename = arg.rsplit(|byte| *byte == b'/').next().unwrap_or(arg);
    basename.eq_ignore_ascii_case(b"ffmpeg") || basename.eq_ignore_ascii_case(b"ffprobe")
}

fn is_internal_media_relay(candidate: &[u8], relay_port: u16) -> bool {
    let Some(rest) = candidate.strip_prefix(b"http://") else {
        return false;
    };
    let Some(authority_end) = rest.iter().position(|byte| *byte == b'/') else {
        return false;
    };
    let authority = &rest[..authority_end];
    let expected_ipv4 = format!("127.0.0.1:{relay_port}");
    let expected_ipv6 = format!("[::1]:{relay_port}");
    if authority != expected_ipv4.as_bytes() && authority != expected_ipv6.as_bytes() {
        return false;
    }
    let Some(token) = rest[authority_end..].strip_prefix(b"/.jellyrin/internal/remote-media/")
    else {
        return false;
    };
    token.len() == 43
        && token
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn find_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| starts_ascii_case_insensitive(window, needle))
}

fn starts_ascii_case_insensitive(value: &[u8], prefix: &[u8]) -> bool {
    value.len() >= prefix.len() && value[..prefix.len()].eq_ignore_ascii_case(prefix)
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::symlink};

    use super::*;

    #[test]
    fn scans_binary_logs_across_read_chunks_without_returning_canaries() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("jellyrin.log");
        let mut bytes = vec![b'x'; READ_CHUNK_BYTES - 3];
        bytes.extend_from_slice(b" https://user-canary:password-canary@provider.invalid/live/user-canary/password-canary/42?access_token=token-canary\xff");
        fs::write(&path, bytes).unwrap();
        let report = audit_runtime_hygiene(RuntimeHygieneAuditOptions {
            log_files: vec![path],
            argv_files: Vec::new(),
            relay_port: None,
        })
        .unwrap();
        assert_eq!(report.exit_code(), 2);
        assert_eq!(report.counts.url_userinfo, 1);
        assert_eq!(report.counts.sensitive_query, 1);
        assert_eq!(report.counts.xtream_credential_path, 1);
        let encoded = serde_json::to_string(&report).unwrap();
        for canary in [
            "user-canary",
            "password-canary",
            "token-canary",
            "provider.invalid",
        ] {
            assert!(!encoded.contains(canary));
        }
    }

    #[test]
    fn media_argv_allows_only_the_exact_loopback_relay() {
        let directory = tempfile::tempdir().unwrap();
        let clean_path = directory.path().join("clean-cmdline");
        fs::write(
            &clean_path,
            b"/usr/local/bin/ffmpeg\0-i\0http://127.0.0.1:8096/.jellyrin/internal/remote-media/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\0",
        )
        .unwrap();
        let dirty_path = directory.path().join("dirty-cmdline");
        fs::write(
            &dirty_path,
            b"ffprobe\0https://provider.invalid/movie/user-canary/password-canary/42\0",
        )
        .unwrap();
        let clean = audit_runtime_hygiene(RuntimeHygieneAuditOptions {
            log_files: Vec::new(),
            argv_files: vec![clean_path],
            relay_port: Some(8096),
        })
        .unwrap();
        assert_eq!(clean.exit_code(), 0);
        let dirty = audit_runtime_hygiene(RuntimeHygieneAuditOptions {
            log_files: Vec::new(),
            argv_files: vec![dirty_path],
            relay_port: Some(8096),
        })
        .unwrap();
        assert_eq!(dirty.exit_code(), 2);
        assert_eq!(dirty.counts.upstream_media_argv, 1);
        assert_eq!(dirty.counts.xtream_credential_path, 1);

        for bypass in [
            "https://127.0.0.1:8096/.jellyrin/internal/remote-media/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "http://localhost:8096/.jellyrin/internal/remote-media/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "http://127.0.0.1:8097/.jellyrin/internal/remote-media/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "prefix=http://127.0.0.1:8096/.jellyrin/internal/remote-media/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "http://127.0.0.1:8096/.jellyrin/internal/remote-media/short",
            "http://127.0.0.1:8096/.jellyrin/internal/remote-media/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA?token=secret",
        ] {
            let path = directory.path().join(format!("bypass-{}", bypass.len()));
            let mut cmdline = b"ffmpeg\0-i\0".to_vec();
            cmdline.extend_from_slice(bypass.as_bytes());
            cmdline.push(0);
            fs::write(&path, cmdline).unwrap();
            let report = audit_runtime_hygiene(RuntimeHygieneAuditOptions {
                log_files: Vec::new(),
                argv_files: vec![path],
                relay_port: Some(8096),
            })
            .unwrap();
            assert_eq!(report.exit_code(), 2, "bypass should fail");
            assert_eq!(report.counts.upstream_media_argv, 1);
        }
    }

    #[test]
    fn argv_requires_a_port_and_a_complete_nul_delimited_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("cmdline");
        fs::write(&path, b"ffmpeg\0-i\0local.ts").unwrap();
        let unterminated = audit_runtime_hygiene(RuntimeHygieneAuditOptions {
            log_files: Vec::new(),
            argv_files: vec![path.clone()],
            relay_port: Some(8096),
        })
        .unwrap();
        assert_eq!(unterminated.exit_code(), 3);

        fs::write(&path, b"ffmpeg\0-i\0local.ts\0").unwrap();
        let no_port = audit_runtime_hygiene(RuntimeHygieneAuditOptions {
            log_files: Vec::new(),
            argv_files: vec![path],
            relay_port: None,
        })
        .unwrap();
        assert_eq!(no_port.exit_code(), 3);
    }

    #[test]
    fn sensitive_query_keys_are_ascii_case_insensitive_and_percent_decoded() {
        for key in [
            b"ToKeN".as_slice(),
            b"%74oken".as_slice(),
            b"access-token".as_slice(),
            b"api%5Fkey".as_slice(),
        ] {
            assert!(is_sensitive_query_key(key));
        }
        assert!(!is_sensitive_query_key(b"title"));
        assert!(!is_sensitive_query_key(b"%zztoken"));
    }

    #[test]
    fn exact_ipv6_relay_is_allowed() {
        let token = b"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
        let mut candidate = b"http://[::1]:8096/.jellyrin/internal/remote-media/".to_vec();
        candidate.extend_from_slice(token);
        assert!(is_internal_media_relay(&candidate, 8096));
        assert!(!is_internal_media_relay(&candidate, 8097));
    }

    #[test]
    fn missing_symlink_and_empty_input_fail_closed_without_paths_in_report() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("target");
        let link = directory.path().join("link");
        fs::write(&target, b"clean").unwrap();
        symlink(&target, &link).unwrap();
        let report = audit_runtime_hygiene(RuntimeHygieneAuditOptions {
            log_files: vec![link, directory.path().join("missing")],
            argv_files: Vec::new(),
            relay_port: None,
        })
        .unwrap();
        assert_eq!(report.exit_code(), 3);
        assert_eq!(report.incomplete_sources, 2);
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains(directory.path().to_str().unwrap())
        );

        let empty = audit_runtime_hygiene(RuntimeHygieneAuditOptions::default()).unwrap();
        assert_eq!(empty.exit_code(), 3);
    }
}
