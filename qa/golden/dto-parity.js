#!/usr/bin/env node

const fs = require('node:fs/promises');
const path = require('node:path');

const repoRoot = path.resolve(__dirname, '..', '..');
const defaultPlansDir = path.resolve(repoRoot, '..', '..', 'plans');
const plansDir = process.env.JELLYRIN_PLANS_DIR || defaultPlansDir;
const generatedDir = path.join(plansDir, 'generated');

async function main() {
  const coverage = await readJson('dto-coverage.json', []);
  const apiParity = await readJson(path.join('golden-traces', 'api-parity-latest.json'), null);
  const browser = {
    p0DirectPlay: await readBrowserTrace('p0-direct-play'),
    loginHome: await readBrowserTrace('login-home'),
  };

  const contexts = buildContexts(apiParity, browser);
  const fields = coverage.map((field) => evaluateField(field, contexts));
  const summary = summarize(fields);
  const report = {
    generatedAt: new Date().toISOString(),
    plansDir,
    summary,
    fields,
  };

  await fs.writeFile(
    path.join(generatedDir, 'dto-field-parity.json'),
    `${JSON.stringify(report, null, 2)}\n`,
  );
  await fs.writeFile(path.join(generatedDir, 'dto-field-parity.md'), renderMarkdown(report));
  console.log(`wrote ${path.join(generatedDir, 'dto-field-parity.md')}`);
  if (summary.failed > 0) {
    process.exitCode = 1;
  }
}

async function readJson(relativePath, fallback) {
  try {
    return JSON.parse(await fs.readFile(path.join(generatedDir, relativePath), 'utf8'));
  } catch (error) {
    if (error.code === 'ENOENT') {
      return fallback;
    }
    throw error;
  }
}

async function readBrowserTrace(flow) {
  const dir = path.join(generatedDir, 'e2e-traces', flow);
  const comparison = await readJson(path.join('e2e-traces', flow, 'comparison.json'), null);
  if (!comparison) {
    return null;
  }
  const targets = {};
  for (const target of ['upstream', 'jellyrin']) {
    targets[target] = await readJsonl(path.join(dir, `${target}.requests.jsonl`));
  }
  return { comparison, targets };
}

async function readJsonl(filePath) {
  try {
    const text = await fs.readFile(filePath, 'utf8');
    return text.trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
  } catch (error) {
    if (error.code === 'ENOENT') {
      return [];
    }
    throw error;
  }
}

function buildContexts(apiParity, browser) {
  const apiByName = Object.fromEntries((apiParity?.results || []).map((result) => [result.name, result]));
  const p0 = browser.p0DirectPlay;
  return {
    playbackInfoRequest: {
      upstream: findRequest(p0, 'upstream', (record) => record.method === 'POST' && /\/PlaybackInfo$/.test(record.path))?.requestPostData,
      jellyrin: findRequest(p0, 'jellyrin', (record) => record.method === 'POST' && /\/PlaybackInfo$/.test(record.path))?.requestPostData,
      source: 'e2e-traces/p0-direct-play/*requests.jsonl PlaybackInfo request',
    },
    playbackInfoResponse: responseShapeContext(p0, (record) => record.method === 'POST' && /\/PlaybackInfo$/.test(record.path), 'e2e-traces/p0-direct-play/*requests.jsonl PlaybackInfo responseShape'),
    mediaSourceInfo: responseShapeContext(p0, (record) => record.method === 'POST' && /\/PlaybackInfo$/.test(record.path), 'e2e-traces/p0-direct-play/*requests.jsonl PlaybackInfo MediaSources responseShape'),
    itemDetail: responseShapeContext(p0, (record) => record.method === 'GET' && /\/Users\/[^/]+\/Items\/[^/]+$/.test(record.path), 'e2e-traces/p0-direct-play/*requests.jsonl item detail responseShape'),
    sessionsPlayingRequest: {
      upstream: findRequest(p0, 'upstream', (record) => record.method === 'POST' && record.path === '/Sessions/Playing')?.requestPostData,
      jellyrin: findRequest(p0, 'jellyrin', (record) => record.method === 'POST' && record.path === '/Sessions/Playing')?.requestPostData,
      source: 'e2e-traces/p0-direct-play/*requests.jsonl Sessions/Playing request',
    },
    publicInfo: apiBodyContext(apiByName['public-info'], 'golden-traces/api-parity-latest.json public-info body'),
    users: apiArrayFirstContext(apiByName.users, 'golden-traces/api-parity-latest.json users body[0]'),
    queryResult: apiBodyContext(apiByName.views || apiByName['items-movies-first-page'], 'golden-traces/api-parity-latest.json QueryResult body'),
    scheduledTask: apiArrayFirstContext(apiByName['scheduled-tasks'], 'golden-traces/api-parity-latest.json scheduled-tasks body[0]'),
  };
}

function responseShapeContext(trace, predicate, source) {
  return {
    upstream: findRequest(trace, 'upstream', predicate)?.responseShape,
    jellyrin: findRequest(trace, 'jellyrin', predicate)?.responseShape,
    source,
  };
}

function apiBodyContext(result, source) {
  return {
    upstream: result?.upstream?.body,
    jellyrin: result?.jellyrin?.body,
    source,
  };
}

function apiArrayFirstContext(result, source) {
  return {
    upstream: Array.isArray(result?.upstream?.body) ? result.upstream.body[0] : undefined,
    jellyrin: Array.isArray(result?.jellyrin?.body) ? result.jellyrin.body[0] : undefined,
    source,
  };
}

function findRequest(trace, target, predicate) {
  const requests = trace?.targets?.[target] || [];
  return requests.find(predicate);
}

function evaluateField(field, contexts) {
  const rule = ruleFor(field);
  if (!rule) {
    return resultFor(field, 'missing-evidence', 'no rule for DTO family/field');
  }
  const context = contexts[rule.context];
  if (!context?.upstream || !context?.jellyrin) {
    return resultFor(field, 'missing-evidence', `missing context ${rule.context}`);
  }
  const upstream = hasAnyPath(context.upstream, rule.paths);
  const jellyrin = hasAnyPath(context.jellyrin, rule.paths);
  if (upstream && jellyrin) {
    return resultFor(field, 'upstream-validated', context.source, rule.paths);
  }
  if (upstream || jellyrin) {
    return resultFor(
      field,
      'partial-golden',
      `${context.source}; upstream=${upstream}, jellyrin=${jellyrin}`,
      rule.paths,
    );
  }
  return resultFor(field, 'missing-evidence', `${context.source}; field absent`, rule.paths);
}

function ruleFor(field) {
  const name = `${field.dto_family}.${field.field}`;
  const exact = {
    'PlaybackInfoRequest.EnableDirectPlay': ['playbackInfoRequest', ['EnableDirectPlay']],
    'PlaybackInfoRequest.EnableDirectStream': ['playbackInfoRequest', ['EnableDirectStream']],
    'PlaybackInfoRequest.EnableTranscoding': ['playbackInfoRequest', ['EnableTranscoding']],
    'PlaybackInfoRequest.MediaSourceId': ['playbackInfoRequest', ['MediaSourceId']],
    'PlaybackInfoRequest.AudioStreamIndex': ['playbackInfoRequest', ['AudioStreamIndex']],
    'PlaybackInfoRequest.SubtitleStreamIndex': ['playbackInfoRequest', ['SubtitleStreamIndex']],
    'PlaybackInfoRequest.StartTimeTicks': ['playbackInfoRequest', ['StartTimeTicks']],
    'PlaybackInfoRequest.StartPositionTicks': ['playbackInfoRequest', ['StartPositionTicks']],
    'PlaybackInfoRequest.DeviceProfile.DirectPlayProfiles': ['playbackInfoRequest', ['DeviceProfile.DirectPlayProfiles']],
    'PlaybackInfoResponse.ErrorCode': ['playbackInfoResponse', ['ErrorCode']],
    'PlaybackInfoResponse.PlaySessionId': ['playbackInfoResponse', ['PlaySessionId']],
    'PlaybackInfoResponse.MediaSources': ['playbackInfoResponse', ['MediaSources']],
    'MediaSourceInfo.SupportsDirectPlay': ['mediaSourceInfo', ['MediaSources.[].SupportsDirectPlay']],
    'MediaSourceInfo.SupportsDirectStream': ['mediaSourceInfo', ['MediaSources.[].SupportsDirectStream']],
    'MediaSourceInfo.SupportsTranscoding': ['mediaSourceInfo', ['MediaSources.[].SupportsTranscoding']],
    'MediaSourceInfo.TranscodingUrl': ['mediaSourceInfo', ['MediaSources.[].TranscodingUrl']],
    'MediaSourceInfo.DirectStreamUrl': ['mediaSourceInfo', ['MediaSources.[].DirectStreamUrl']],
    'MediaSourceInfo.RunTimeTicks': ['mediaSourceInfo', ['MediaSources.[].RunTimeTicks']],
    'MediaSourceInfo.Bitrate': ['mediaSourceInfo', ['MediaSources.[].Bitrate']],
    'MediaSourceInfo.DefaultAudioStreamIndex': ['mediaSourceInfo', ['MediaSources.[].DefaultAudioStreamIndex']],
    'MediaSourceInfo.DefaultSubtitleStreamIndex': ['mediaSourceInfo', ['MediaSources.[].DefaultSubtitleStreamIndex']],
    'MediaSourceInfo.MediaStreams': ['mediaSourceInfo', ['MediaSources.[].MediaStreams']],
    'MediaStream.Index': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].Index']],
    'MediaStream.Type': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].Type']],
    'MediaStream.Codec': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].Codec']],
    'MediaStream.Width': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].Width']],
    'MediaStream.Height': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].Height']],
    'MediaStream.Channels': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].Channels']],
    'MediaStream.SampleRate': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].SampleRate']],
    'MediaStream.FrameRate': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].AverageFrameRate', 'MediaSources.[].MediaStreams.[].RealFrameRate']],
    'MediaStream.Profile': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].Profile']],
    'MediaStream.PixelFormat': ['mediaSourceInfo', ['MediaSources.[].MediaStreams.[].PixelFormat']],
    'PlayState.MediaSourceId': ['sessionsPlayingRequest', ['MediaSourceId']],
    'PlayState.AudioStreamIndex': ['sessionsPlayingRequest', ['AudioStreamIndex']],
    'PlayState.SubtitleStreamIndex': ['sessionsPlayingRequest', ['SubtitleStreamIndex']],
    'PlayState.PositionTicks': ['sessionsPlayingRequest', ['PositionTicks']],
    'PlayState.IsPaused': ['sessionsPlayingRequest', ['IsPaused']],
    'UserDto.Id': ['users', ['Id']],
    'UserDto.Name': ['users', ['Name']],
    'UserDto.ServerId': ['users', ['ServerId']],
    'UserDto.HasPassword': ['users', ['HasPassword']],
    'UserDto.HasConfiguredPassword': ['users', ['HasConfiguredPassword']],
    'UserDto.HasConfiguredEasyPassword': ['users', ['HasConfiguredEasyPassword']],
    'UserDto.Policy': ['users', ['Policy']],
    'UserDto.Configuration': ['users', ['Configuration']],
    'UserPolicy.IsAdministrator': ['users', ['Policy.IsAdministrator']],
    'UserPolicy.EnableContentDeletion': ['users', ['Policy.EnableContentDeletion']],
    'UserPolicy.EnableRemoteControlOfOtherUsers': ['users', ['Policy.EnableRemoteControlOfOtherUsers']],
    'UserPolicy.EnableSharedDeviceControl': ['users', ['Policy.EnableSharedDeviceControl']],
    'UserPolicy.EnableLiveTvManagement': ['users', ['Policy.EnableLiveTvManagement']],
    'UserPolicy.EnableLiveTvAccess': ['users', ['Policy.EnableLiveTvAccess']],
    'UserPolicy.EnableMediaPlayback': ['users', ['Policy.EnableMediaPlayback']],
    'UserPolicy.EnableAudioPlaybackTranscoding': ['users', ['Policy.EnableAudioPlaybackTranscoding']],
    'UserPolicy.EnableVideoPlaybackTranscoding': ['users', ['Policy.EnableVideoPlaybackTranscoding']],
    'PublicSystemInfo.Version': ['publicInfo', ['Version']],
    'PublicSystemInfo.ServerName': ['publicInfo', ['ServerName']],
    'PublicSystemInfo.Id': ['publicInfo', ['Id']],
    'PublicSystemInfo.LocalAddress': ['publicInfo', ['LocalAddress']],
    'PublicSystemInfo.StartupWizardCompleted': ['publicInfo', ['StartupWizardCompleted']],
    'QueryResult<T>.Items': ['queryResult', ['Items']],
    'QueryResult<T>.TotalRecordCount': ['queryResult', ['TotalRecordCount']],
    'QueryResult<T>.StartIndex': ['queryResult', ['StartIndex']],
    'ScheduledTaskInfo.Name': ['scheduledTask', ['Name']],
    'ScheduledTaskInfo.Key': ['scheduledTask', ['Key']],
    'ScheduledTaskInfo.State': ['scheduledTask', ['State']],
    'ScheduledTaskInfo.Id': ['scheduledTask', ['Id']],
    'ScheduledTaskInfo.LastExecutionResult': ['scheduledTask', ['LastExecutionResult']],
    'ScheduledTaskInfo.Triggers': ['scheduledTask', ['Triggers']],
    'BaseItemDto.Id': ['itemDetail', ['Id']],
    'BaseItemDto.Name': ['itemDetail', ['Name']],
    'BaseItemDto.Overview': ['itemDetail', ['Overview']],
    'BaseItemDto.Type': ['itemDetail', ['Type']],
    'BaseItemDto.RunTimeTicks': ['itemDetail', ['RunTimeTicks']],
    'BaseItemDto.MediaSources': ['itemDetail', ['MediaSources']],
    'BaseItemDto.ImageTags': ['itemDetail', ['ImageTags']],
    'BaseItemDto.UserData': ['itemDetail', ['UserData']],
  };
  const value = exact[name];
  return value ? { context: value[0], paths: value[1] } : null;
}

function resultFor(field, goldenStatus, evidence, paths = []) {
  return {
    dto_family: field.dto_family,
    field: field.field,
    direction: field.direction,
    implementationStatus: field.status,
    goldenStatus,
    evidence,
    paths,
    danger_if_wrong: field.danger_if_wrong,
    test_name: field.test_name,
  };
}

function hasAnyPath(value, paths) {
  return paths.some((fieldPath) => hasPath(value, fieldPath.split('.')));
}

function hasPath(value, parts) {
  if (parts.length === 0) {
    return value !== undefined;
  }
  const [part, ...rest] = parts;
  if (part === '[]') {
    return Array.isArray(value) && value.some((child) => hasPath(child, rest));
  }
  if (Array.isArray(value)) {
    return value.some((child) => hasPath(child, parts));
  }
  if (!value || typeof value !== 'object' || !(part in value)) {
    return false;
  }
  return hasPath(value[part], rest);
}

function summarize(fields) {
  const byGoldenStatus = {};
  const byFamily = {};
  for (const field of fields) {
    byGoldenStatus[field.goldenStatus] = (byGoldenStatus[field.goldenStatus] || 0) + 1;
    byFamily[field.dto_family] ||= { total: 0, upstreamValidated: 0, partialGolden: 0, missingEvidence: 0 };
    byFamily[field.dto_family].total += 1;
    if (field.goldenStatus === 'upstream-validated') {
      byFamily[field.dto_family].upstreamValidated += 1;
    } else if (field.goldenStatus === 'partial-golden') {
      byFamily[field.dto_family].partialGolden += 1;
    } else {
      byFamily[field.dto_family].missingEvidence += 1;
    }
  }
  const upstreamValidated = byGoldenStatus['upstream-validated'] || 0;
  return {
    total: fields.length,
    upstreamValidated,
    partialGolden: byGoldenStatus['partial-golden'] || 0,
    missingEvidence: byGoldenStatus['missing-evidence'] || 0,
    failed: 0,
    upstreamValidatedPercent: percent(upstreamValidated, fields.length),
    byGoldenStatus,
    byFamily,
  };
}

function percent(value, total) {
  if (!total) {
    return 0;
  }
  return Number(((value / total) * 100).toFixed(1));
}

function renderMarkdown(report) {
  const lines = [];
  lines.push('# DTO Field Parity');
  lines.push('');
  lines.push(`Generated: ${report.generatedAt}`);
  lines.push('');
  lines.push('## Summary');
  lines.push('');
  lines.push(`- Total fields: ${report.summary.total}`);
  lines.push(`- Upstream/Jellyrin golden validated: ${report.summary.upstreamValidated} (${report.summary.upstreamValidatedPercent}%)`);
  lines.push(`- Partial golden evidence: ${report.summary.partialGolden}`);
  lines.push(`- Missing golden evidence: ${report.summary.missingEvidence}`);
  lines.push('');
  lines.push('## By Family');
  lines.push('');
  lines.push('| DTO Family | Total | Upstream Validated | Partial | Missing |');
  lines.push('| --- | ---: | ---: | ---: | ---: |');
  for (const [family, data] of Object.entries(report.summary.byFamily)) {
    lines.push(`| ${family} | ${data.total} | ${data.upstreamValidated} | ${data.partialGolden} | ${data.missingEvidence} |`);
  }
  lines.push('');
  lines.push('## Field Evidence');
  lines.push('');
  lines.push('| DTO Field | Status | Evidence |');
  lines.push('| --- | --- | --- |');
  for (const field of report.fields) {
    lines.push(`| ${field.dto_family}.${field.field} | \`${field.goldenStatus}\` | ${field.evidence} |`);
  }
  lines.push('');
  lines.push('## Notes');
  lines.push('');
  lines.push('- `upstream-validated` means the field is present in both upstream Jellyfin and Jellyrin evidence for the mapped DTO context.');
  lines.push('- `partial-golden` means only one side exposes it in the current golden evidence; this is actionable parity work or a context mismatch.');
  lines.push('- `missing-evidence` means the current traces do not cover that field yet.');
  lines.push('- This report reads sanitized API golden bodies and browser request response shapes; it does not copy tokens or passwords.');
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error);
  process.exit(1);
});
