#!/usr/bin/env node

import fs from 'node:fs/promises';
import path from 'node:path';

const upstreamBaseUrl = trim(process.env.JELLYFIN_UPSTREAM_URL || 'http://127.0.0.1:8096');
const targetBaseUrl = trim(process.env.JELLYRIN_URL || 'http://127.0.0.1:8097');
const username = process.env.JELLYFIN_AUDIT_USER || process.env.JELLYFIN_ADMIN_USER || 'joe';
const password = requiredPassword(
  process.env.JELLYFIN_AUDIT_PASSWORD || process.env.JELLYFIN_ADMIN_PASSWORD,
  'JELLYFIN_AUDIT_PASSWORD or JELLYFIN_ADMIN_PASSWORD',
);
const outputDir = path.resolve(process.cwd(), process.env.JELLYFIN_AUDIT_OUT_DIR || 'output');
const generatedAt = new Date().toISOString();

const detailFields = [
  'Overview',
  'PrimaryImageAspectRatio',
  'Genres',
  'Studios',
  'People',
  'ProviderIds',
  'Tags',
  'RemoteTrailers',
  'MediaSources',
  'MediaStreams',
  'ChildCount',
  'RecursiveItemCount',
  'ProductionYear',
  'PremiereDate',
  'EndDate',
  'CommunityRating',
  'OfficialRating',
  'RunTimeTicks',
  'SeriesName',
  'SeasonName',
  'IndexNumber',
  'ParentIndexNumber',
  'SortName',
  'Path',
].join(',');

const listFields = [
  'PrimaryImageAspectRatio',
  'BasicSyncInfo',
  'ProductionYear',
  'PremiereDate',
  'EndDate',
  'Overview',
  'Genres',
  'Studios',
  'People',
  'ProviderIds',
  'Tags',
  'RemoteTrailers',
  'CommunityRating',
  'OfficialRating',
  'RunTimeTicks',
  'ChildCount',
  'RecursiveItemCount',
  'SeriesName',
  'SeasonName',
  'IndexNumber',
  'ParentIndexNumber',
  'SortName',
  'MediaSources',
  'MediaStreams',
].join(',');

const uiCriticalFields = [
  'Name',
  'Type',
  'ImageTags.Primary',
  'ImageTags.Logo',
  'BackdropImageTags',
  'PrimaryImageAspectRatio',
  'ProductionYear',
  'PremiereDate',
  'EndDate',
  'CommunityRating',
  'OfficialRating',
  'Overview',
  'Genres',
  'Studios',
  'People',
  'ProviderIds.Imdb',
  'ProviderIds.Tmdb',
  'ProviderIds.Tvdb',
  'RemoteTrailers',
  'Tags',
  'ChildCount',
  'RecursiveItemCount',
  'UserData',
];

const playbackProfile = {
  DirectPlayProfiles: [],
  TranscodingProfiles: [{
    Container: 'ts',
    Type: 'Video',
    AudioCodec: 'aac,mp3,ac3,eac3,opus,flac',
    VideoCodec: 'h264,hevc,mpeg4,mpeg2video',
    Context: 'Streaming',
    Protocol: 'hls',
    MaxAudioChannels: '2',
    MinSegments: '1',
    BreakOnNonKeyFrames: false,
  }],
  ContainerProfiles: [],
  CodecProfiles: [],
  SubtitleProfiles: [
    { Format: 'vtt', Method: 'External' },
    { Format: 'ass', Method: 'Embed' },
    { Format: 'srt', Method: 'Embed' },
    { Format: 'pgssub', Method: 'Embed' },
  ],
};

async function main() {
  const [upstream, target] = await Promise.all([
    authenticate('upstream', upstreamBaseUrl),
    authenticate('target', targetBaseUrl),
  ]);

  const report = {
    generatedAt,
    upstreamBaseUrl,
    targetBaseUrl,
    user: username,
    sections: [],
    globalChecks: [],
    summary: {},
  };

  const [upstreamViews, targetViews] = await Promise.all([
    getJson(upstream, '/UserViews', { UserId: upstream.userId }),
    getJson(target, '/UserViews', { UserId: target.userId }),
  ]);
  const upstreamItems = array(upstreamViews.body?.Items);
  const targetItems = array(targetViews.body?.Items);

  report.globalChecks.push(compareStatus('UserViews', upstreamViews, targetViews));
  report.globalChecks.push(...compareRequiredFields('UserViews item', upstreamItems[0], targetItems[0], [
    'Id',
    'Name',
    'Type',
    'CollectionType',
    'ImageTags.Primary',
  ]));

  const targetViewByKey = new Map(targetItems.map((view) => [viewKey(view), view]));
  const targetViewByCollection = new Map(targetItems.map((view) => [view.CollectionType || view.Name, view]));

  for (const upstreamView of upstreamItems) {
    const targetView = targetViewByKey.get(viewKey(upstreamView))
      || targetViewByCollection.get(upstreamView.CollectionType || upstreamView.Name)
      || targetItems.find((candidate) => sameName(candidate.Name, upstreamView.Name));
    const section = await auditView(upstream, target, upstreamView, targetView);
    report.sections.push(section);
  }

  const samples = collectSamples(report.sections);
  report.globalChecks.push(...await auditMetadataSurfaces(upstream, target, samples));
  report.globalChecks.push(...await auditPlayback(upstream, target, samples));
  report.globalChecks.push(...await auditLookupSurfaces(upstream, target, samples));

  const allIssues = [
    ...report.globalChecks.filter((check) => !check.ok),
    ...report.sections.flatMap((section) => section.issues),
  ];
  report.summary = {
    sections: report.sections.length,
    checks: report.globalChecks.length + report.sections.reduce((sum, section) => sum + section.checks, 0),
    failedChecks: allIssues.length,
    high: allIssues.filter((issue) => issue.severity === 'high').length,
    medium: allIssues.filter((issue) => issue.severity === 'medium').length,
    low: allIssues.filter((issue) => issue.severity === 'low').length,
  };

  await fs.mkdir(outputDir, { recursive: true });
  const jsonPath = path.join(outputDir, 'jellyfin-section-parity-audit.json');
  const mdPath = path.join(outputDir, 'jellyfin-section-parity-audit.md');
  await fs.writeFile(jsonPath, `${JSON.stringify(report, null, 2)}\n`);
  await fs.writeFile(mdPath, renderMarkdown(report));

  console.log(`wrote ${jsonPath}`);
  console.log(`wrote ${mdPath}`);
  console.log(`sections=${report.summary.sections} failed=${report.summary.failedChecks} high=${report.summary.high} medium=${report.summary.medium} low=${report.summary.low}`);
  if (report.summary.high > 0) {
    process.exitCode = 1;
  }
}

async function auditView(upstream, target, upstreamView, targetView) {
  const section = {
    name: upstreamView.Name,
    collectionType: upstreamView.CollectionType || null,
    upstreamViewId: upstreamView.Id,
    targetViewId: targetView?.Id || null,
    checks: 0,
    counts: {},
    probes: [],
    samples: [],
    issues: [],
  };

  if (!targetView) {
    section.issues.push(issue('high', section.name, 'view-missing', 'La vista existe en 8096 pero no en 8097', {
      upstream: pick(upstreamView, ['Name', 'Type', 'CollectionType', 'Id']),
    }));
    return section;
  }

  const [upstreamPage, targetPage] = await Promise.all([
    getItems(upstream, { ParentId: upstreamView.Id, Limit: 80 }),
    getItems(target, { ParentId: targetView.Id, Limit: 80 }),
  ]);
  section.checks += 1;
  section.counts.upstreamTotal = upstreamPage.body?.TotalRecordCount ?? null;
  section.counts.targetTotal = targetPage.body?.TotalRecordCount ?? null;
  if (upstreamPage.status !== targetPage.status) {
    section.issues.push(issue('high', section.name, 'view-items-status', `Items status ${upstreamPage.status} != ${targetPage.status}`, {
      upstreamPath: upstreamPage.path,
      targetPath: targetPage.path,
    }));
    return section;
  }

  const upstreamList = array(upstreamPage.body?.Items);
  const targetList = array(targetPage.body?.Items);
  if (targetList.length === 0 && upstreamList.length > 0) {
    section.issues.push(issue('high', section.name, 'view-empty', 'La vista tiene elementos en 8096 pero 8097 devuelve lista vacia', {
      upstreamCount: upstreamList.length,
      targetCount: targetList.length,
    }));
  }

  const targetByIdentity = indexByIdentity(targetList);
  for (const upstreamItem of upstreamList.slice(0, 30)) {
    const matched = findMatch(upstreamItem, targetByIdentity, targetList);
    section.checks += 1;
    if (!matched) {
      section.issues.push(issue('medium', section.name, 'item-missing', `No encuentro en 8097 un item equivalente a ${label(upstreamItem)}`, {
        upstream: signature(upstreamItem),
      }));
      continue;
    }
    section.samples.push({
      upstream: signature(upstreamItem),
      target: signature(matched),
    });
    section.issues.push(...compareUpstreamRequiredFields(section.name, 'list-item-fields', upstreamItem, matched, uiCriticalFields));
    section.issues.push(...await compareImages(section.name, upstream, target, upstreamItem, matched));
  }

  const representative = chooseRepresentative(upstreamList, targetList, targetByIdentity, section.collectionType);
  if (representative) {
    const [upstreamDetail, targetDetail] = await Promise.all([
      getItem(upstream, representative.upstream.Id),
      getItem(target, representative.target.Id),
    ]);
    section.checks += 1;
    if (upstreamDetail.status !== targetDetail.status) {
      section.issues.push(issue('high', section.name, 'detail-status', `Detalle status ${upstreamDetail.status} != ${targetDetail.status}`, {
        upstream: signature(representative.upstream),
        target: signature(representative.target),
      }));
    } else {
      section.issues.push(...compareUpstreamRequiredFields(section.name, 'detail-fields', upstreamDetail.body, targetDetail.body, uiCriticalFields));
      section.issues.push(...await compareImages(section.name, upstream, target, upstreamDetail.body, targetDetail.body));
      if (upstreamDetail.body?.Type === 'Series') {
        section.issues.push(...await auditSeries(section.name, upstream, target, upstreamDetail.body, targetDetail.body));
      }
      if (array(upstreamDetail.body?.People).length > 0) {
        section.issues.push(...await auditPeopleFromItem(section.name, upstream, target, upstreamDetail.body, targetDetail.body));
      }
    }
  }

  section.issues.push(...await auditTypedView(section, upstream, target, upstreamView, targetView));

  return section;
}

async function auditTypedView(section, upstream, target, upstreamView, targetView) {
  const issues = [];
  const includeTypesByCollection = {
    tvshows: ['Series'],
    movies: ['Movie'],
    music: ['MusicAlbum', 'Audio'],
    boxsets: ['BoxSet'],
    playlists: ['Playlist'],
  };
  const includeTypes = includeTypesByCollection[section.collectionType];
  if (!includeTypes) {
    return issues;
  }
  for (const includeType of includeTypes) {
    const [upstreamPage, targetPage] = await Promise.all([
      getItems(upstream, { ParentId: upstreamView.Id, Recursive: true, IncludeItemTypes: includeType, Limit: 80 }),
      getItems(target, { ParentId: targetView.Id, Recursive: true, IncludeItemTypes: includeType, Limit: 80 }),
    ]);
    section.checks += 1;
    const probe = {
      includeType,
      upstreamTotal: upstreamPage.body?.TotalRecordCount ?? null,
      targetTotal: targetPage.body?.TotalRecordCount ?? null,
      matched: 0,
    };
    section.probes.push(probe);
    if (upstreamPage.status !== targetPage.status) {
      issues.push(issue('high', section.name, `typed-${includeType}-status`, `${includeType} recursive status ${upstreamPage.status} != ${targetPage.status}`, {
        upstreamPath: upstreamPage.path,
        targetPath: targetPage.path,
      }));
      continue;
    }
    const upstreamItems = array(upstreamPage.body?.Items);
    const targetItems = array(targetPage.body?.Items);
    if (upstreamItems.length > 0 && targetItems.length === 0) {
      issues.push(issue('high', section.name, `typed-${includeType}-empty`, `8096 devuelve ${includeType} en esta vista, 8097 no devuelve ninguno`, {
        upstreamCount: upstreamItems.length,
        targetCount: targetItems.length,
      }));
    }
    const targetByIdentity = indexByIdentity(targetItems);
    for (const upstreamItem of upstreamItems.slice(0, 30)) {
      const targetItem = findMatch(upstreamItem, targetByIdentity, targetItems);
      section.checks += 1;
      if (!targetItem) {
        issues.push(issue('medium', section.name, `typed-${includeType}-missing`, `No encuentro ${includeType} equivalente a ${label(upstreamItem)} en el probe recursivo`, {
          upstream: signature(upstreamItem),
        }));
        continue;
      }
      probe.matched += 1;
      pushUniqueSample(section.samples, {
        upstream: signature(upstreamItem),
        target: signature(targetItem),
      });
      issues.push(...compareUpstreamRequiredFields(section.name, `typed-${includeType}-fields`, upstreamItem, targetItem, uiCriticalFields));
      issues.push(...await compareImages(section.name, upstream, target, upstreamItem, targetItem));
      if (includeType === 'Series') {
        const [upstreamDetail, targetDetail] = await Promise.all([
          getItem(upstream, upstreamItem.Id),
          getItem(target, targetItem.Id),
        ]);
        if (upstreamDetail.status !== targetDetail.status) {
          issues.push(issue('high', section.name, 'typed-series-detail-status', `Detalle de serie ${upstreamDetail.status} != ${targetDetail.status}`, {
            upstream: signature(upstreamItem),
            target: signature(targetItem),
          }));
        } else {
          issues.push(...compareUpstreamRequiredFields(section.name, 'typed-series-detail-fields', upstreamDetail.body, targetDetail.body, uiCriticalFields));
          issues.push(...await compareImages(section.name, upstream, target, upstreamDetail.body, targetDetail.body));
          issues.push(...await auditSeries(section.name, upstream, target, upstreamDetail.body, targetDetail.body));
          issues.push(...await auditPeopleFromItem(section.name, upstream, target, upstreamDetail.body, targetDetail.body));
        }
      }
    }
  }
  return issues;
}

async function auditSeries(sectionName, upstream, target, upstreamSeries, targetSeries) {
  const issues = [];
  const [upstreamSeasons, targetSeasons] = await Promise.all([
    getJson(upstream, `/Shows/${encodeURIComponent(upstreamSeries.Id)}/Seasons`, { UserId: upstream.userId, Fields: listFields }),
    getJson(target, `/Shows/${encodeURIComponent(targetSeries.Id)}/Seasons`, { UserId: target.userId, Fields: listFields }),
  ]);
  if (upstreamSeasons.status !== targetSeasons.status) {
    issues.push(issue('high', sectionName, 'series-seasons-status', `Seasons status ${upstreamSeasons.status} != ${targetSeasons.status}`, {
      series: upstreamSeries.Name,
    }));
    return issues;
  }
  const upstreamSeasonItems = array(upstreamSeasons.body?.Items);
  const targetSeasonItems = array(targetSeasons.body?.Items);
  if (upstreamSeasonItems.length !== targetSeasonItems.length) {
    issues.push(issue('medium', sectionName, 'series-season-count', `Temporadas ${upstreamSeasonItems.length} != ${targetSeasonItems.length}`, {
      series: upstreamSeries.Name,
    }));
  }
  const targetSeasonByIndex = new Map(targetSeasonItems.map((season) => [String(season.IndexNumber ?? season.Name), season]));
  for (const upstreamSeason of upstreamSeasonItems.slice(0, 3)) {
    const targetSeason = targetSeasonByIndex.get(String(upstreamSeason.IndexNumber ?? upstreamSeason.Name))
      || targetSeasonItems.find((candidate) => sameName(candidate.Name, upstreamSeason.Name));
    if (!targetSeason) {
      issues.push(issue('medium', sectionName, 'season-missing', `No encuentro temporada equivalente a ${label(upstreamSeason)}`, {
        series: upstreamSeries.Name,
      }));
      continue;
    }
    issues.push(...compareUpstreamRequiredFields(sectionName, 'season-fields', upstreamSeason, targetSeason, uiCriticalFields));
    issues.push(...await auditEpisodes(sectionName, upstream, target, upstreamSeries, targetSeries, upstreamSeason, targetSeason));
  }
  return issues;
}

async function auditEpisodes(sectionName, upstream, target, upstreamSeries, targetSeries, upstreamSeason, targetSeason) {
  const [upstreamEpisodes, targetEpisodes] = await Promise.all([
    getJson(upstream, `/Shows/${encodeURIComponent(upstreamSeries.Id)}/Episodes`, { UserId: upstream.userId, SeasonId: upstreamSeason.Id, Fields: listFields }),
    getJson(target, `/Shows/${encodeURIComponent(targetSeries.Id)}/Episodes`, { UserId: target.userId, SeasonId: targetSeason.Id, Fields: listFields }),
  ]);
  const issues = [];
  if (upstreamEpisodes.status !== targetEpisodes.status) {
    issues.push(issue('high', sectionName, 'season-episodes-status', `Episodes status ${upstreamEpisodes.status} != ${targetEpisodes.status}`, {
      series: upstreamSeries.Name,
      season: upstreamSeason.Name,
    }));
    return issues;
  }
  const upstreamItems = array(upstreamEpisodes.body?.Items);
  const targetItems = array(targetEpisodes.body?.Items);
  const upstreamOrder = upstreamItems.map(episodeOrderKey);
  const targetOrder = targetItems.map(episodeOrderKey);
  if (upstreamOrder.join('|') !== targetOrder.join('|')) {
    issues.push(issue('high', sectionName, 'episode-order', 'El orden de episodios no coincide con 8096', {
      series: upstreamSeries.Name,
      season: upstreamSeason.Name,
      upstreamOrder: upstreamOrder.slice(0, 20),
      targetOrder: targetOrder.slice(0, 20),
    }));
  }
  const targetByIdentity = indexByIdentity(targetItems);
  for (const upstreamEpisode of upstreamItems.slice(0, 12)) {
    const targetEpisode = findMatch(upstreamEpisode, targetByIdentity, targetItems);
    if (!targetEpisode) {
      issues.push(issue('medium', sectionName, 'episode-missing', `No encuentro episodio equivalente a ${label(upstreamEpisode)}`, {
        series: upstreamSeries.Name,
        season: upstreamSeason.Name,
      }));
      continue;
    }
    issues.push(...compareUpstreamRequiredFields(sectionName, 'episode-fields', upstreamEpisode, targetEpisode, [
      'Name',
      'SeriesName',
      'SeasonName',
      'IndexNumber',
      'ParentIndexNumber',
      'ProductionYear',
      'PremiereDate',
      'ImageTags.Primary',
      'PrimaryImageAspectRatio',
      'Overview',
      'MediaSources',
      'MediaStreams',
      'RunTimeTicks',
      'UserData',
    ]));
  }
  return issues;
}

async function auditPeopleFromItem(sectionName, upstream, target, upstreamItem, targetItem) {
  const issues = [];
  const targetPeopleByName = new Map(array(targetItem?.People).map((person) => [person.Name, person]));
  for (const upstreamPerson of array(upstreamItem?.People).slice(0, 8)) {
    const targetPerson = targetPeopleByName.get(upstreamPerson.Name);
    if (!targetPerson) {
      issues.push(issue('medium', sectionName, 'person-missing-in-detail', `Falta en reparto/equipo: ${upstreamPerson.Name}`, {
        item: upstreamItem.Name,
      }));
      continue;
    }
    issues.push(...compareUpstreamRequiredFields(sectionName, 'person-inline-fields', upstreamPerson, targetPerson, [
      'Name',
      'Id',
      'Role',
      'Type',
      'PrimaryImageTag',
    ]));
  }
  return issues;
}

async function auditMetadataSurfaces(upstream, target, samples) {
  const checks = [];
  const items = samples.filter(Boolean).slice(0, 8);
  for (const sample of items) {
    for (const endpoint of [
      { name: 'MetadataEditor', path: (id) => `/Items/${encodeURIComponent(id)}/MetadataEditor`, fields: ['ExternalIdInfos', 'Cultures', 'Countries'] },
      { name: 'ExternalIdInfos', path: (id) => `/Items/${encodeURIComponent(id)}/ExternalIdInfos`, fields: ['[].Key', '[].Name'] },
      { name: 'Images', path: (id) => `/Items/${encodeURIComponent(id)}/Images`, fields: ['[].ImageType', '[].ImageTag'] },
      { name: 'RemoteImagesProviders', path: (id) => `/Items/${encodeURIComponent(id)}/RemoteImages/Providers`, fields: ['[]'] },
    ]) {
      const [upstreamResponse, targetResponse] = await Promise.all([
        getJson(upstream, endpoint.path(sample.upstream.Id)),
        getJson(target, endpoint.path(sample.target.Id)),
      ]);
      checks.push(compareStatus(`${endpoint.name} ${sample.upstream.Name}`, upstreamResponse, targetResponse));
      if (upstreamResponse.status === 200 && targetResponse.status === 200) {
        checks.push(...compareRequiredFields(`${endpoint.name} ${sample.upstream.Name}`, upstreamResponse.body, targetResponse.body, endpoint.fields));
      }
    }
  }
  return checks;
}

async function auditLookupSurfaces(upstream, target, samples) {
  const checks = [];
  const firstNamed = samples.find((sample) => sample?.upstream?.Name);
  const firstPeopleName = samples
    .map((sample) => array(sample.upstream?.People)[0]?.Name)
    .find(Boolean);
  const probes = [
    { name: 'Genres', params: { UserId: (side) => side.userId, Limit: 10 } },
    { name: 'Studios', params: { UserId: (side) => side.userId, Limit: 10 } },
    { name: 'Years', params: { UserId: (side) => side.userId, Limit: 10 } },
    { name: 'Persons', params: { UserId: (side) => side.userId, SearchTerm: firstPeopleName || '', Limit: 10 } },
    { name: 'Items search', path: '/Items', params: { UserId: (side) => side.userId, Recursive: true, SearchTerm: firstNamed?.upstream?.Name || '', Fields: listFields, Limit: 10 } },
  ];
  for (const probe of probes) {
    const pathName = probe.path || `/${probe.name}`;
    const [upstreamResponse, targetResponse] = await Promise.all([
      getJson(upstream, pathName, materializeParams(probe.params, upstream)),
      getJson(target, pathName, materializeParams(probe.params, target)),
    ]);
    checks.push(compareStatus(probe.name, upstreamResponse, targetResponse));
    if (upstreamResponse.status === 200 && targetResponse.status === 200) {
      checks.push(...compareRequiredFields(probe.name, upstreamResponse.body, targetResponse.body, ['Items', 'TotalRecordCount']));
    }
  }
  return checks;
}

async function auditPlayback(upstream, target, samples) {
  const checks = [];
  const playable = samples.find((sample) => ['Movie', 'Episode', 'Video'].includes(sample?.upstream?.Type));
  if (!playable) {
    checks.push({ ok: true, severity: 'low', section: 'Playback', code: 'playback-skipped', message: 'No hay sample reproducible para comparar PlaybackInfo' });
    return checks;
  }
  const upstreamStreams = array(playable.upstream.MediaStreams);
  const targetStreams = array(playable.target.MediaStreams);
  const upstreamAudio = upstreamStreams.find((stream) => stream.Type === 'Audio')?.Index;
  const targetAudio = targetStreams.find((stream) => stream.Type === 'Audio')?.Index;
  const upstreamSubtitle = upstreamStreams.find((stream) => stream.Type === 'Subtitle')?.Index ?? -1;
  const targetSubtitle = targetStreams.find((stream) => stream.Type === 'Subtitle')?.Index ?? -1;
  const upstreamBody = {
    UserId: upstream.userId,
    MediaSourceId: playable.upstream.Id,
    AudioStreamIndex: upstreamAudio,
    SubtitleStreamIndex: upstreamSubtitle,
    EnableDirectPlay: false,
    EnableDirectStream: false,
    EnableTranscoding: true,
    StartPositionTicks: 0,
    DeviceProfile: playbackProfile,
  };
  const targetBody = {
    UserId: target.userId,
    MediaSourceId: playable.target.Id,
    AudioStreamIndex: targetAudio,
    SubtitleStreamIndex: targetSubtitle,
    EnableDirectPlay: false,
    EnableDirectStream: false,
    EnableTranscoding: true,
    StartPositionTicks: 0,
    DeviceProfile: playbackProfile,
  };
  const [upstreamResponse, targetResponse] = await Promise.all([
    postJson(upstream, `/Items/${encodeURIComponent(playable.upstream.Id)}/PlaybackInfo`, upstreamBody),
    postJson(target, `/Items/${encodeURIComponent(playable.target.Id)}/PlaybackInfo`, targetBody),
  ]);
  checks.push(compareStatus(`PlaybackInfo ${playable.upstream.Name}`, upstreamResponse, targetResponse));
  if (upstreamResponse.status === 200 && targetResponse.status === 200) {
    checks.push(...compareRequiredFields(`PlaybackInfo ${playable.upstream.Name}`, upstreamResponse.body, targetResponse.body, [
      'MediaSources',
      'MediaSources.0.MediaStreams',
      'MediaSources.0.TranscodingUrl',
      'PlaySessionId',
    ]));
    const upstreamMediaSource = upstreamResponse.body?.MediaSources?.[0];
    const targetMediaSource = targetResponse.body?.MediaSources?.[0];
    checks.push(...compareStreamInventory(playable.upstream.Name, upstreamMediaSource, targetMediaSource));
  }
  return checks;
}

async function compareImages(sectionName, upstream, target, upstreamItem, targetItem) {
  const issues = [];
  for (const type of ['Primary', 'Logo', 'Backdrop']) {
    const upstreamHas = hasImage(upstreamItem, type);
    if (!upstreamHas) {
      continue;
    }
    if (!hasImage(targetItem, type)) {
      issues.push(issue(type === 'Primary' ? 'high' : 'medium', sectionName, `image-${type.toLowerCase()}-missing`, `${label(upstreamItem)} tiene imagen ${type} en 8096 pero no en DTO 8097`, {
        upstream: signature(upstreamItem),
        target: signature(targetItem),
      }));
      continue;
    }
    const [upstreamImage, targetImage] = await Promise.all([
      getRaw(upstream, imagePath(upstreamItem, type)),
      getRaw(target, imagePath(targetItem, type)),
    ]);
    if (upstreamImage.status < 400 && targetImage.status >= 400) {
      issues.push(issue(type === 'Primary' ? 'high' : 'medium', sectionName, `image-${type.toLowerCase()}-http`, `${label(targetItem)} anuncia imagen ${type} pero la ruta devuelve HTTP ${targetImage.status}`, {
        upstreamStatus: upstreamImage.status,
        targetStatus: targetImage.status,
        targetPath: targetImage.path,
      }));
    }
  }
  return issues;
}

function compareStatus(section, upstreamResponse, targetResponse) {
  if (upstreamResponse.status !== targetResponse.status) {
    return issue('high', section, 'status-mismatch', `HTTP ${upstreamResponse.status} en 8096 vs HTTP ${targetResponse.status} en 8097`, {
      upstreamPath: upstreamResponse.path,
      targetPath: targetResponse.path,
    });
  }
  return { ok: true, section, code: 'status-ok', message: `HTTP ${targetResponse.status}` };
}

function compareRequiredFields(section, upstreamBody, targetBody, fields) {
  const checks = [];
  for (const field of fields) {
    if (!pathHasMeaningfulValue(upstreamBody, field)) {
      continue;
    }
    if (!pathHasMeaningfulValue(targetBody, field)) {
      checks.push(issue('medium', section, 'field-missing', `8096 trae ${field} pero 8097 no`, {
        field,
        upstreamValue: previewValue(getPath(upstreamBody, field)),
        targetValue: previewValue(getPath(targetBody, field)),
      }));
    }
  }
  return checks;
}

function compareUpstreamRequiredFields(section, code, upstreamBody, targetBody, fields) {
  return fields
    .filter((field) => pathHasMeaningfulValue(upstreamBody, field) && !pathHasMeaningfulValue(targetBody, field))
    .map((field) => issue(field.includes('ImageTags.Primary') ? 'high' : 'medium', section, code, `${label(upstreamBody)}: 8096 trae ${field} pero 8097 no`, {
      field,
      upstreamValue: previewValue(getPath(upstreamBody, field)),
      targetValue: previewValue(getPath(targetBody, field)),
    }));
}

function compareStreamInventory(name, upstreamMediaSource, targetMediaSource) {
  const issues = [];
  const upstreamStreams = array(upstreamMediaSource?.MediaStreams);
  const targetStreams = array(targetMediaSource?.MediaStreams);
  for (const type of ['Video', 'Audio', 'Subtitle']) {
    const upstreamCount = upstreamStreams.filter((stream) => stream.Type === type).length;
    const targetCount = targetStreams.filter((stream) => stream.Type === type).length;
    if (upstreamCount !== targetCount) {
      issues.push(issue(type === 'Audio' ? 'high' : 'medium', 'Playback', 'stream-count', `${name}: streams ${type} ${upstreamCount} != ${targetCount}`, {
        upstreamCount,
        targetCount,
      }));
    }
  }
  for (const stream of upstreamStreams) {
    const targetStream = targetStreams.find((candidate) => candidate.Type === stream.Type && candidate.Index === stream.Index)
      || targetStreams.find((candidate) => candidate.Type === stream.Type && sameName(candidate.DisplayTitle, stream.DisplayTitle));
    if (!targetStream) {
      continue;
    }
    for (const field of ['Type', 'Index', 'Codec', 'Language', 'DisplayTitle', 'Channels', 'IsDefault', 'IsForced']) {
      if (pathHasMeaningfulValue(stream, field) && !pathHasMeaningfulValue(targetStream, field)) {
        issues.push(issue('medium', 'Playback', 'stream-field-missing', `${name}: stream ${stream.Type}/${stream.Index} trae ${field} en 8096 pero no en 8097`, {
          field,
          upstream: previewValue(stream),
          target: previewValue(targetStream),
        }));
      }
    }
  }
  return issues;
}

async function authenticate(labelName, baseUrl) {
  const response = await fetch(`${baseUrl}/Users/AuthenticateByName`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: 'MediaBrowser Client="Jellyrin Section Audit", Device="Harness", DeviceId="jellyrin-section-audit", Version="dev"',
    },
    body: JSON.stringify({ Username: username, Pw: password }),
  });
  if (!response.ok) {
    throw new Error(`${labelName} auth failed at ${baseUrl}: HTTP ${response.status} ${await response.text()}`);
  }
  const body = await response.json();
  return {
    label: labelName,
    baseUrl,
    token: body.AccessToken,
    userId: body.User?.Id,
    serverId: body.ServerId,
  };
}

async function getItems(side, params) {
  return getJson(side, '/Items', {
    UserId: side.userId,
    Fields: listFields,
    StartIndex: 0,
    ...params,
  });
}

async function getItem(side, id) {
  return getJson(side, `/Users/${encodeURIComponent(side.userId)}/Items/${encodeURIComponent(id)}`, {
    Fields: detailFields,
  });
}

async function getJson(side, pathname, params = {}) {
  return request(side, 'GET', pathname, params);
}

async function postJson(side, pathname, body) {
  return request(side, 'POST', pathname, {}, body);
}

async function getRaw(side, pathname, params = {}) {
  return request(side, 'GET', pathname, params, undefined, true);
}

async function request(side, method, pathname, params = {}, body = undefined, raw = false) {
  const url = new URL(`${side.baseUrl}${pathname}`);
  for (const [key, value] of Object.entries(params)) {
    if (value !== undefined && value !== null && value !== '') {
      url.searchParams.set(key, String(value));
    }
  }
  const response = await fetch(url, {
    method,
    headers: {
      'X-Emby-Token': side.token,
      ...(body === undefined ? {} : { 'Content-Type': 'application/json' }),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const contentType = response.headers.get('content-type') || '';
  let parsedBody = null;
  if (!raw) {
    const text = await response.text();
    if (text && contentType.includes('application/json')) {
      parsedBody = JSON.parse(text);
    } else {
      parsedBody = text;
    }
  }
  return {
    status: response.status,
    contentType,
    path: `${url.pathname}${url.search}`,
    body: parsedBody,
  };
}

function collectSamples(sections) {
  const samples = [];
  for (const section of sections) {
    for (const sample of section.samples) {
      samples.push(sample);
    }
  }
  const byType = new Map();
  for (const sample of samples) {
    if (!sample?.upstream?.Type || byType.has(sample.upstream.Type)) {
      continue;
    }
    byType.set(sample.upstream.Type, sample);
  }
  const preferred = ['Series', 'Season', 'Episode', 'Movie', 'Audio', 'MusicAlbum', 'Person']
    .map((type) => byType.get(type))
    .filter(Boolean);
  return [...preferred, ...samples].filter((sample, index, array) => array.findIndex((candidate) => candidate?.upstream?.Id === sample?.upstream?.Id) === index);
}

function pushUniqueSample(samples, sample) {
  if (!sample?.upstream?.Id || samples.some((candidate) => candidate?.upstream?.Id === sample.upstream.Id)) {
    return;
  }
  samples.push(sample);
}

function chooseRepresentative(upstreamList, targetList, targetByIdentity, collectionType) {
  const preferredTypes = collectionType === 'tvshows'
    ? ['Series', 'Season', 'Episode']
    : collectionType === 'music'
      ? ['MusicAlbum', 'Audio', 'MusicArtist']
      : ['Movie', 'Series', 'Episode', 'Audio', 'Video', 'Person'];
  for (const type of preferredTypes) {
    const upstream = upstreamList.find((item) => item.Type === type);
    if (!upstream) {
      continue;
    }
    const target = findMatch(upstream, targetByIdentity, targetList);
    if (target) {
      return { upstream, target };
    }
  }
  const upstream = upstreamList[0];
  const target = upstream ? findMatch(upstream, targetByIdentity, targetList) : null;
  return upstream && target ? { upstream, target } : null;
}

function indexByIdentity(items) {
  const map = new Map();
  for (const item of items) {
    for (const key of identityKeys(item)) {
      if (!map.has(key)) {
        map.set(key, item);
      }
    }
  }
  return map;
}

function findMatch(item, index, candidates) {
  for (const key of identityKeys(item)) {
    const matched = index.get(key);
    if (matched) {
      return matched;
    }
  }
  return candidates.find((candidate) => candidate.Type === item.Type && sameName(candidate.Name, item.Name))
    || candidates.find((candidate) => sameName(candidate.Name, item.Name));
}

function identityKeys(item) {
  const keys = [];
  const name = norm(item.Name);
  const type = item.Type || '';
  if (name) {
    keys.push(`${type}|${name}|${item.ProductionYear ?? ''}`);
    keys.push(`${type}|${name}|${item.ParentIndexNumber ?? ''}|${item.IndexNumber ?? ''}`);
    keys.push(`${type}|${name}|${norm(item.SeriesName)}|${item.ParentIndexNumber ?? ''}|${item.IndexNumber ?? ''}`);
    keys.push(`${type}|${name}`);
  }
  if (item.ProviderIds?.Imdb) keys.push(`${type}|imdb|${norm(item.ProviderIds.Imdb)}`);
  if (item.ProviderIds?.Tmdb) keys.push(`${type}|tmdb|${norm(item.ProviderIds.Tmdb)}`);
  if (item.ProviderIds?.Tvdb) keys.push(`${type}|tvdb|${norm(item.ProviderIds.Tvdb)}`);
  return keys;
}

function hasImage(item, type) {
  if (type === 'Backdrop') {
    return array(item?.BackdropImageTags).length > 0 || Boolean(item?.ImageTags?.Backdrop);
  }
  return Boolean(item?.ImageTags?.[type] || item?.[`${type}ImageTag`]);
}

function imagePath(item, type) {
  const imageType = type === 'Backdrop' ? 'Backdrop/0' : type;
  return `/Items/${encodeURIComponent(item.Id)}/Images/${imageType}`;
}

function viewKey(view) {
  return `${norm(view.CollectionType || '')}|${norm(view.Name || '')}`;
}

function materializeParams(params, side) {
  const out = {};
  for (const [key, value] of Object.entries(params || {})) {
    out[key] = typeof value === 'function' ? value(side) : value;
  }
  return out;
}

function compareRequiredArrayPath(value, part) {
  if (part === '[]') {
    return Array.isArray(value) && value.length > 0;
  }
  if (!part.startsWith('[].')) {
    return undefined;
  }
  const field = part.slice(3);
  return Array.isArray(value) && value.some((entry) => pathHasMeaningfulValue(entry, field));
}

function pathHasMeaningfulValue(value, fieldPath) {
  if (fieldPath === '[]' || fieldPath.startsWith('[].')) {
    return compareRequiredArrayPath(value, fieldPath);
  }
  const found = getPath(value, fieldPath);
  if (found === undefined || found === null) return false;
  if (typeof found === 'string') return found.trim().length > 0;
  if (Array.isArray(found)) return found.length > 0;
  if (typeof found === 'object') return Object.keys(found).length > 0;
  return true;
}

function getPath(value, fieldPath) {
  if (!fieldPath) return value;
  const parts = fieldPath.split('.');
  let current = value;
  for (const part of parts) {
    if (current === undefined || current === null) return undefined;
    if (/^\d+$/.test(part)) {
      current = current[Number(part)];
    } else if (part === '[]') {
      current = Array.isArray(current) ? current[0] : undefined;
    } else {
      current = current[part];
    }
  }
  return current;
}

function issue(severity, section, code, message, evidence = {}) {
  return { ok: false, severity, section, code, message, evidence };
}

function signature(item) {
  return pick(item, [
    'Id',
    'Name',
    'Type',
    'SeriesName',
    'SeasonName',
    'ParentIndexNumber',
    'IndexNumber',
    'ProductionYear',
    'PremiereDate',
    'EndDate',
    'ImageTags',
    'BackdropImageTags',
    'ProviderIds',
    'People',
    'MediaStreams',
  ]);
}

function pick(item, fields) {
  const out = {};
  for (const field of fields) {
    const value = getPath(item, field);
    if (value !== undefined) out[field] = value;
  }
  return out;
}

function previewValue(value) {
  if (value === undefined) return undefined;
  const text = JSON.stringify(value);
  return text && text.length > 400 ? `${text.slice(0, 400)}...` : value;
}

function episodeOrderKey(item) {
  const season = item.ParentIndexNumber ?? '?';
  const episode = item.IndexNumber ?? '?';
  return `S${season}:E${episode}:${item.Name}`;
}

function label(item) {
  return `${item?.Type || 'Item'} "${item?.Name || item?.Id || '?'}"`;
}

function array(value) {
  return Array.isArray(value) ? value : [];
}

function sameName(left, right) {
  return norm(left) === norm(right);
}

function norm(value) {
  return String(value ?? '').normalize('NFKD').replace(/[\u0300-\u036f]/g, '').toLowerCase().trim();
}

function trim(value) {
  return value.replace(/\/+$/, '');
}

function requiredPassword(value, labelName) {
  if (!value) {
    throw new Error(`${labelName} must be set`);
  }
  return value;
}

function renderMarkdown(report) {
  const lines = [];
  lines.push('# Jellyfin 8096 vs Jellyrin 8097 section parity audit');
  lines.push('');
  lines.push(`Generated: ${report.generatedAt}`);
  lines.push(`Upstream: ${report.upstreamBaseUrl}`);
  lines.push(`Target: ${report.targetBaseUrl}`);
  lines.push('');
  lines.push('## Summary');
  lines.push('');
  lines.push(`- Sections audited: ${report.summary.sections}`);
  lines.push(`- Checks: ${report.summary.checks}`);
  lines.push(`- Failed checks: ${report.summary.failedChecks}`);
  lines.push(`- High: ${report.summary.high}`);
  lines.push(`- Medium: ${report.summary.medium}`);
  lines.push(`- Low: ${report.summary.low}`);
  lines.push('');
  lines.push('## Global issues');
  lines.push('');
  const globalIssues = report.globalChecks.filter((check) => !check.ok);
  if (globalIssues.length === 0) {
    lines.push('- None');
  } else {
    for (const check of globalIssues) {
      lines.push(`- ${check.severity.toUpperCase()} ${check.section} ${check.code}: ${check.message}`);
    }
  }
  lines.push('');
  lines.push('## Sections');
  for (const section of report.sections) {
    lines.push('');
    lines.push(`### ${section.name} (${section.collectionType || 'no collection type'})`);
    lines.push('');
    lines.push(`- Upstream view: ${section.upstreamViewId}`);
    lines.push(`- Target view: ${section.targetViewId || 'missing'}`);
    lines.push(`- Upstream total: ${section.counts.upstreamTotal ?? 'n/a'}`);
    lines.push(`- Target total: ${section.counts.targetTotal ?? 'n/a'}`);
    lines.push(`- Samples matched: ${section.samples.length}`);
    if (section.probes.length > 0) {
      lines.push(`- Typed probes: ${section.probes.map((probe) => `${probe.includeType} ${probe.upstreamTotal ?? 'n/a'} vs ${probe.targetTotal ?? 'n/a'} matched ${probe.matched}`).join('; ')}`);
    }
    if (section.issues.length === 0) {
      lines.push('- Issues: none');
    } else {
      lines.push('- Issues:');
      for (const entry of section.issues.slice(0, 80)) {
        lines.push(`  - ${entry.severity.toUpperCase()} ${entry.code}: ${entry.message}`);
      }
      if (section.issues.length > 80) {
        lines.push(`  - ... ${section.issues.length - 80} more in JSON`);
      }
    }
  }
  lines.push('');
  lines.push('## Notes');
  lines.push('');
  lines.push('- The audit compares DTO/API surfaces used by Jellyfin web. It maps target items by type, name, provider ids and episode indexes because ids can differ between servers.');
  lines.push('- A missing field is only reported when 8096 returns a meaningful value and 8097 does not.');
  lines.push('- The JSON file contains request paths and evidence snippets for each finding.');
  lines.push('');
  return `${lines.join('\n')}\n`;
}

main().catch((error) => {
  console.error(error.stack || error.message || String(error));
  process.exitCode = 1;
});
