#!/usr/bin/env node

'use strict';

const http = require('node:http');

// Pre-encoded 1-second MPEG-2 video TS segment (176×144 @ 29.97 fps, 100 kbps).
// Generated with: ffmpeg -f lavfi -i color=black:size=176x144:rate=29.97:duration=1
//   -c:v mpeg2video -b:v 100k -r 29.97 -f mpegts <output>
// This is served as a looping chunked stream so that:
//   1. ffprobe (run by Jellyfin PlaybackInfo) can identify "mpeg2video" within its 3-second
//      analyzeduration window — proper PAT/PMT and I-frame data are present from byte 0.
//   2. The upstream Jellyfin SharedHttpStream temp file stays open (no EOF is sent) so the
//      /LiveTv/LiveStreamFiles/<id>/stream.ts endpoint is fetcheable for the byte-check probe.
// Content-Type: video/mp2t, no Content-Length (chunked transfer encoding).
const TS_LOOP_BODY = Buffer.from(
  'R0AREABC8CUAAcEAAP8B/wAB/IAUSBIBBkZGbXBlZwlTZXJ2aWNlMDF3fEPK////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//////////////////////9HQAAQAACwDQABwQAAAAHwACqxBLL/////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/////////////////////////////////////////////0dQABAAArASAAHBAADhAPAAAuEA8ACe'
  + 'iyPR////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////R0EAMAdQ'
  + 'AAB7DH4AAAAB4AAAgMAKMQAH79cRAAfYYQAAAbMLAJAU///gEAAAAbUUigABAAAAAAG4AAgAQAAA'
  + 'AQAAD//4AAABtY//80GAAAABARP4fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5Sl'
  + 'Ii5SlIiAAAABAhP4fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIiAAAAB'
  + 'AxP4fSlIi5SlIi5HAQARUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIgAAAAQQT+H0p'
  + 'SIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIgAAAAQUT+H0pSIuUpSIuUpSI'
  + 'uUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIgAAAAQYT+H0pSIuUpSIuUpSIuUpSIuUpSIuU'
  + 'pSIuUpSIuUpSIuUpSIuUpSIuUpSIgAAAAQcT+EcBADItAP//////////////////////////////'
  + '////////////////////////////fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5Sl'
  + 'Ii5SlIiAAAABCBP4fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIiAAAAB'
  + 'CRP4fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIiAR0EAM0oA////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/////////////wAAAeAAAIDACjEACQdNEQAH79cAAAEAAFf/+4AAAAG1gR/zQYAAAAEBEnCzgAAA'
  + 'AQIScLOAAAABAxJws4AAAAEEEnCzgAAAAQUScLOAAAABBhJws4AAAAEHEnCzgAAAAQgScLOAAAAB'
  + 'CRJws4BHQQA0ShAAAIbHfgD/////////////////////////////////////////////////////'
  + '////////////////////////////////////AAAB4AAAgMAKMQAJHsMRAAkHTQAAAQAAl//7gAAA'
  + 'AbWBH/NBgAAAAQEScLOAAAABAhJws4AAAAEDEnCzgAAAAQQScLOAAAABBRJws4AAAAEGEnCzgAAA'
  + 'AQcScLOAAAABCBJws4AAAAEJEnCzgEdAABEAALANAAHBAAAAAfAAKrEEsv//////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////R1AAEQACsBIAAcEAAOEA8AAC'
  + '4QDwAJ6LI9H/////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//////////////////////////////////////////////////////////////////////////9H'
  + 'QQA1SgD/////////////////////////////////////////////////////////////////////'
  + '////////////////////////////AAAB4AAAgMAKMQAJNjkRAAkewwAAAQAA1//7gAAAAbWBH/NB'
  + 'gAAAAQEScLOAAAABAhJws4AAAAEDEnCzgAAAAQQScLOAAAABBRJws4AAAAEGEnCzgAAAAQcScLOA'
  + 'AAABCBJws4AAAAEJEnCzgEdBADZKEAAAkoJ+AP//////////////////////////////////////'
  + '//////////////////////////////////////////////////8AAAHgAACAwAoxAAlNrxEACTY5'
  + 'AAABAAEX//uAAAABtYEf80GAAAABARJws4AAAAECEnCzgAAAAQMScLOAAAABBBJws4AAAAEFEnCz'
  + 'gAAAAQYScLOAAAABBxJws4AAAAEIEnCzgAAAAQkScLOAR0EAN0oA////////////////////////'
  + '/////////////////////////////////////////////////////////////////////////wAA'
  + 'AeAAAIDACjEACWUlEQAJTa8AAAEAAVf/+4AAAAG1gR/zQYAAAAEBEnCzgAAAAQIScLOAAAABAxJw'
  + 's4AAAAEEEnCzgAAAAQUScLOAAAABBhJws4AAAAEHEnCzgAAAAQgScLOAAAABCRJws4BHQAASAACw'
  + 'DQABwQAAAAHwACqxBLL/////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/////////////0dQABIAArASAAHBAADhAPAAAuEA8ACeiyPR////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////R0EAOEoQAACePX4A////////////////////////'
  + '/////////////////////////////////////////////////////////////////wAAAeAAAIDA'
  + 'CjEACXybEQAJZSUAAAEAAZf/+4AAAAG1gR/zQYAAAAEBEnCzgAAAAQIScLOAAAABAxJws4AAAAEE'
  + 'EnCzgAAAAQUScLOAAAABBhJws4AAAAEHEnCzgAAAAQgScLOAAAABCRJws4BHQQA5SgD/////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////AAAB4AAAgMAKMQAJlBERAAl8mwAAAQAB1//7gAAAAbWBH/NBgAAAAQEScLOAAAAB'
  + 'AhJws4AAAAEDEnCzgAAAAQQScLOAAAABBRJws4AAAAEGEnCzgAAAAQcScLOAAAABCBJws4AAAAEJ'
  + 'EnCzgEdBADpKEAAAqfh+AP//////////////////////////////////////////////////////'
  + '//////////////////////////////////8AAAHgAACAwAoxAAmrhxEACZQRAAABAAIX//uAAAAB'
  + 'tYEf80GAAAABARJws4AAAAECEnCzgAAAAQMScLOAAAABBBJws4AAAAEFEnCzgAAAAQYScLOAAAAB'
  + 'BxJws4AAAAEIEnCzgAAAAQkScLOAR0AAEwAAsA0AAcEAAAAB8AAqsQSy////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//////////////////////////////////////////////////9HUAATAAKwEgABwQAA4QDwAALh'
  + 'APAAnosj0f//////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/////////////////////////////////////////////////////////////////////////0dB'
  + 'ADtKAP//////////////////////////////////////////////////////////////////////'
  + '//////////////////////////8AAAHgAACAwAoxAAnC/REACauHAAABAAJX//uAAAABtYEf80GA'
  + 'AAABARJws4AAAAECEnCzgAAAAQMScLOAAAABBBJws4AAAAEFEnCzgAAAAQYScLOAAAABBxJws4AA'
  + 'AAEIEnCzgAAAAQkScLOAR0EAPEoQAAC1s34A////////////////////////////////////////'
  + '/////////////////////////////////////////////////wAAAeAAAIDACjEACdpzEQAJwv0A'
  + 'AAEAApf/+4AAAAG1gR/zQYAAAAEBEnCzgAAAAQIScLOAAAABAxJws4AAAAEEEnCzgAAAAQUScLOA'
  + 'AAABBhJws4AAAAEHEnCzgAAAAQgScLOAAAABCRJws4BHQQA9SgD/////////////////////////'
  + '////////////////////////////////////////////////////////////////////////AAAB'
  + '4AAAgMAKMQAJ8ekRAAnacwAAAQAC1//7gAAAAbWBH/NBgAAAAQEScLOAAAABAhJws4AAAAEDEnCz'
  + 'gAAAAQQScLOAAAABBRJws4AAAAEGEnCzgAAAAQcScLOAAAABCBJws4AAAAEJEnCzgEdAABQAALAN'
  + 'AAHBAAAAAfAAKrEEsv//////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////R1AAFAACsBIAAcEAAOEA8AAC4QDwAJ6LI9H/////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//////////////////////////////////9HQQA+B1AAAMFufgAAAAHgAACAwAoxAAsJXxEACfHp'
  + 'AAABswsAkBT//+AQAAABtRSKAAEAAAAAAbgACAYAAAABAAAP//gAAAG1j//zQYAAAAEBE/h9KUiL'
  + 'lKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiIAAAAECE/h9KUiLlKUiLlKUiLlK'
  + 'UiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiIAAAAEDE/h9KUiLlKUiLkcBAB9SlIi5SlIi5SlI'
  + 'i5SlIi5SlIi5SlIi5SlIi5SlIi5SlIiAAAABBBP4fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5'
  + 'SlIi5SlIi5SlIi5SlIiAAAABBRP4fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5Sl'
  + 'Ii5SlIiAAAABBhP4fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIiAAAAB'
  + 'BxP4RwEAMC0A//////////////////////////////////////////////////////////99KUiL'
  + 'lKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiIAAAAEIE/h9KUiLlKUiLlKUiLlK'
  + 'UiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiLlKUiIAAAAEJE/h9KUiLlKUiLlKUiLlKUiLlKUiLlKUi'
  + 'LlKUiLlKUiLlKUiLlKUiLlKUiIBHQQAxSgD/////////////////////////////////////////'
  + '////////////////////////////////////////////////////////AAAB4AAAgMAKMQALINUR'
  + 'AAsJXwAAAQAAV//7gAAAAbWBH/NBgAAAAQEScLOAAAABAhJws4AAAAEDEnCzgAAAAQQScLOAAAAB'
  + 'BRJws4AAAAEGEnCzgAAAAQcScLOAAAABCBJws4AAAAEJEnCzgEdBADJKEAAAzSl+AP//////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//8AAAHgAACAwAoxAAs4SxEACyDVAAABAACX//uAAAABtYEf80GAAAABARJws4AAAAECEnCzgAAA'
  + 'AQMScLOAAAABBBJws4AAAAEFEnCzgAAAAQYScLOAAAABBxJws4AAAAEIEnCzgAAAAQkScLOAR0AR'
  + 'EQBC8CUAAcEAAP8B/wAB/IAUSBIBBkZGbXBlZwlTZXJ2aWNlMDF3fEPK////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//////////////////9HQAAVAACwDQABwQAAAAHwACqxBLL/////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/////////////////////////////////////////0dQABUAArASAAHBAADhAPAAAuEA8ACeiyPR'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////R0EAM0oA////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/////////////////wAAAeAAAIDACjEAC0/BEQALOEsAAAEAANf/+4AAAAG1gR/zQYAAAAEBEnCz'
  + 'gAAAAQIScLOAAAABAxJws4AAAAEEEnCzgAAAAQUScLOAAAABBhJws4AAAAEHEnCzgAAAAQgScLOA'
  + 'AAABCRJws4BHQQA0ShAAANjkfgD/////////////////////////////////////////////////'
  + '////////////////////////////////////////AAAB4AAAgMAKMQALZzcRAAtPwQAAAQABF//7'
  + 'gAAAAbWBH/NBgAAAAQEScLOAAAABAhJws4AAAAEDEnCzgAAAAQQScLOAAAABBRJws4AAAAEGEnCz'
  + 'gAAAAQcScLOAAAABCBJws4AAAAEJEnCzgEdBADVKAP//////////////////////////////////'
  + '//////////////////////////////////////////////////////////////8AAAHgAACAwAox'
  + 'AAt+rREAC2c3AAABAAFX//uAAAABtYEf80GAAAABARJws4AAAAECEnCzgAAAAQMScLOAAAABBBJw'
  + 's4AAAAEFEnCzgAAAAQYScLOAAAABBxJws4AAAAEIEnCzgAAAAQkScLOAR0AAFgAAsA0AAcEAAAAB'
  + '8AAqsQSy////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//9HUAAWAAKwEgABwQAA4QDwAALhAPAAnosj0f//////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/////////////////////////0dBADZKEAAA5J9+AP//////////////////////////////////'
  + '//////////////////////////////////////////////////////8AAAHgAACAwAoxAAuWIxEA'
  + 'C36tAAABAAGX//uAAAABtYEf80GAAAABARJws4AAAAECEnCzgAAAAQMScLOAAAABBBJws4AAAAEF'
  + 'EnCzgAAAAQYScLOAAAABBxJws4AAAAEIEnCzgAAAAQkScLOAR0EAN0oA////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/wAAAeAAAIDACjEAC62ZEQALliMAAAEAAdf/+4AAAAG1gR/zQYAAAAEBEnCzgAAAAQIScLOAAAAB'
  + 'AxJws4AAAAEEEnCzgAAAAQUScLOAAAABBhJws4AAAAEHEnCzgAAAAQgScLOAAAABCRJws4BHQQA4'
  + 'ShAAAPBafgD/////////////////////////////////////////////////////////////////'
  + '////////////////////////AAAB4AAAgMAKMQALxQ8RAAutmQAAAQACF//7gAAAAbWBH/NBgAAA'
  + 'AQEScLOAAAABAhJws4AAAAEDEnCzgAAAAQQScLOAAAABBRJws4AAAAEGEnCzgAAAAQcScLOAAAAB'
  + 'CBJws4AAAAEJEnCzgEdAABcAALANAAHBAAAAAfAAKrEEsv//////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////R1AAFwACsBIAAcEAAOEA8AAC4QDwAJ6LI9H/'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//////////////////////////////////////////////////////////////9HQQA5SgD/////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////AAAB4AAAgMAKMQAL3IURAAvFDwAAAQACV//7gAAAAbWBH/NBgAAAAQEScLOA'
  + 'AAABAhJws4AAAAEDEnCzgAAAAQQScLOAAAABBRJws4AAAAEGEnCzgAAAAQcScLOAAAABCBJws4AA'
  + 'AAEJEnCzgEdBADpKEAAA/BV+AP//////////////////////////////////////////////////'
  + '//////////////////////////////////////8AAAHgAACAwAoxAAvz+xEAC9yFAAABAAKX//uA'
  + 'AAABtYEf80GAAAABARJws4AAAAECEnCzgAAAAQMScLOAAAABBBJws4AAAAEFEnCzgAAAAQYScLOA'
  + 'AAABBxJws4AAAAEIEnCzgAAAAQkScLOAR0EAO0oA////////////////////////////////////'
  + '/////////////////////////////////////////////////////////////wAAAeAAAIDACjEA'
  + 'DQtxEQAL8/sAAAEAAtf/+4AAAAG1gR/zQYAAAAEBEnCzgAAAAQIScLOAAAABAxJws4AAAAEEEnCz'
  + 'gAAAAQUScLOAAAABBhJws4AAAAEHEnCzgAAAAQgScLOAAAABCRJws4BHQAAYAACwDQABwQAAAAHw'
  + 'ACqxBLL/////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '/0dQABgAArASAAHBAADhAPAAAuEA8ACeiyPR////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////R0EAPAdQAAEH0H4AAAAB4AAAgMAKMQANIucRAA0LcQAAAbMLAJAU'
  + '///gEAAAAbUUigABAAAAAAG4AAgMAAAAAQAAD//4AAABtY//80GAAAABARP4fSlIi5SlIi5SlIi5'
  + 'SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIiAAAABAhP4fSlIi5SlIi5SlIi5SlIi5SlIi5Sl'
  + 'Ii5SlIi5SlIi5SlIi5SlIi5SlIiAAAABAxP4fSlIi5SlIi5HAQAdUpSIuUpSIuUpSIuUpSIuUpSI'
  + 'uUpSIuUpSIuUpSIuUpSIgAAAAQQT+H0pSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuU'
  + 'pSIuUpSIgAAAAQUT+H0pSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIgAAA'
  + 'AQYT+H0pSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIuUpSIgAAAAQcT+EcBAD4t'
  + 'AP//////////////////////////////////////////////////////////fSlIi5SlIi5SlIi5'
  + 'SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIiAAAABCBP4fSlIi5SlIi5SlIi5SlIi5SlIi5Sl'
  + 'Ii5SlIi5SlIi5SlIi5SlIi5SlIiAAAABCRP4fSlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi5SlIi'
  + '5SlIi5SlIi5SlIiAR0EAP0oA////////////////////////////////////////////////////'
  + '/////////////////////////////////////////////wAAAeAAAIDACjEADTpdEQANIucAAAEA'
  + 'AFf/+4AAAAG1gR/zQYAAAAEBEnCzgAAAAQIScLOAAAABAxJws4AAAAEEEnCzgAAAAQUScLOAAAAB'
  + 'BhJws4AAAAEHEnCzgAAAAQgScLOAAAABCRJws4BHQQAwShAAAROLfgD/////////////////////'
  + '////////////////////////////////////////////////////////////////////AAAB4AAA'
  + 'gMAKMQANUdMRAA06XQAAAQAAl//7gAAAAbWBH/NBgAAAAQEScLOAAAABAhJws4AAAAEDEnCzgAAA'
  + 'AQQScLOAAAABBRJws4AAAAEGEnCzgAAAAQcScLOAAAABCBJws4AAAAEJEnCzgEdAABkAALANAAHB'
  + 'AAAAAfAAKrEEsv//////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////R1AAGQACsBIAAcEAAOEA8AAC4QDwAJ6LI9H/////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//////////////////////////////9HQQAxSgD/////////////////////////////////////'
  + '////////////////////////////////////////////////////////////AAAB4AAAgMAKMQAN'
  + 'aUkRAA1R0wAAAQAA1//7gAAAAbWBH/NBgAAAAQEScLOAAAABAhJws4AAAAEDEnCzgAAAAQQScLOA'
  + 'AAABBRJws4AAAAEGEnCzgAAAAQcScLOAAAABCBJws4AAAAEJEnCzgEdBADJKEAABH0Z+AP//////'
  + '////////////////////////////////////////////////////////////////////////////'
  + '//////8AAAHgAACAwAoxAA2AvxEADWlJAAABAAEX//uAAAABtYEf80GAAAABARJws4AAAAECEnCz'
  + 'gAAAAQMScLOAAAABBBJws4AAAAEFEnCzgAAAAQYScLOAAAABBxJws4AAAAEIEnCzgAAAAQkScLOA'
  + 'R0EAM0oA////////////////////////////////////////////////////////////////////'
  + '/////////////////////////////wAAAeAAAIDACjEADZg1EQANgL8AAAEAAVf/+4AAAAG1gR/z'
  + 'QYAAAAEBEnCzgAAAAQIScLOAAAABAxJws4AAAAEEEnCzgAAAAQUScLOAAAABBhJws4AAAAEHEnCz'
  + 'gAAAAQgScLOAAAABCRJws4A=',
  'base64',
);

const LINEUP_CHANNELS = [
  {
    GuideNumber: '4.1',
    GuideName: 'NBC HD',
    VideoCodec: 'mpeg2video',
    AudioCodec: 'ac3',
    HD: 1,
    Favorite: 0,
    DRM: 0,
  },
  {
    GuideNumber: '5.1',
    GuideName: 'CBS HD',
    VideoCodec: 'mpeg2video',
    AudioCodec: 'ac3',
    HD: 1,
    Favorite: 1,
    DRM: 0,
  },
  {
    GuideNumber: '6.1',
    GuideName: 'Pay-Per-View',
    VideoCodec: 'mpeg2video',
    AudioCodec: 'ac3',
    HD: 0,
    Favorite: 0,
    DRM: 1,
  },
];

function buildLineupWithUrls(baseUrl) {
  return LINEUP_CHANNELS.map((channel) => ({
    ...channel,
    URL: `${baseUrl}/auto/v${channel.GuideNumber}`,
  }));
}

function buildDiscoverResponse(baseUrl) {
  return {
    FriendlyName: 'Jellyrin HDHomeRun Simulator',
    ModelNumber: 'HDHR5-4K',
    FirmwareName: 'hdhomerun4_atsc',
    FirmwareVersion: '20220818',
    DeviceID: 'JELLYRIN1',
    DeviceAuth: '',
    BaseURL: baseUrl,
    LineupURL: `${baseUrl}/lineup.json`,
    TunerCount: 4,
  };
}

function start(port) {
  return new Promise((resolve, reject) => {
    const requestedPort = port || 0;
    const tempServer = http.createServer();
    tempServer.listen(requestedPort, '127.0.0.1', () => {
      const assignedPort = tempServer.address().port;
      tempServer.close(() => {
        const baseUrl = `http://127.0.0.1:${assignedPort}`;
        const discover = buildDiscoverResponse(baseUrl);

        // Concurrent connection counters keyed by channel path (e.g. '/auto/v4.1').
        // currentConcurrentByChannel: number of live connections right now.
        // maxConcurrentByChannel: peak concurrent seen since last reset.
        const currentConcurrentByChannel = Object.create(null);
        const maxConcurrentByChannel = Object.create(null);

        function channelConnOpen(channelPath) {
          currentConcurrentByChannel[channelPath] = (currentConcurrentByChannel[channelPath] || 0) + 1;
          if ((maxConcurrentByChannel[channelPath] || 0) < currentConcurrentByChannel[channelPath]) {
            maxConcurrentByChannel[channelPath] = currentConcurrentByChannel[channelPath];
          }
        }

        function channelConnClose(channelPath) {
          currentConcurrentByChannel[channelPath] = Math.max(0, (currentConcurrentByChannel[channelPath] || 1) - 1);
        }

        const server = http.createServer((req, res) => {
          const url = req.url.split('?')[0];

          if (url === '/discover.json') {
            const body = JSON.stringify(discover);
            res.writeHead(200, {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(body),
            });
            res.end(body);
            return;
          }

          if (url === '/lineup.json') {
            const body = JSON.stringify(buildLineupWithUrls(baseUrl));
            res.writeHead(200, {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(body),
            });
            res.end(body);
            return;
          }

          if (url === '/stats') {
            // Return current and max concurrent connections by channel path.
            const body = JSON.stringify({
              maxConcurrentByChannel: Object.assign(Object.create(null), maxConcurrentByChannel),
              currentConcurrentByChannel: Object.assign(Object.create(null), currentConcurrentByChannel),
            });
            res.writeHead(200, {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(body),
            });
            res.end(body);
            return;
          }

          if (url === '/stats/reset' && req.method === 'POST') {
            // Clear max counters (current remains real).
            Object.keys(maxConcurrentByChannel).forEach((k) => { delete maxConcurrentByChannel[k]; });
            const body = JSON.stringify({ ok: true });
            res.writeHead(200, {
              'Content-Type': 'application/json',
              'Content-Length': Buffer.byteLength(body),
            });
            res.end(body);
            return;
          }

          if (url.startsWith('/auto/v')) {
            // Track this incoming connection for sharing metrics.
            channelConnOpen(url);

            // Stream the TS body in a loop (chunked, no Content-Length) until the client
            // disconnects. Looping proper MPEG-2 TS data lets ffprobe identify the codec within
            // its 3-second analyzeduration window without hanging on an empty or null-PID stream.
            // The infinite stream keeps the Jellyfin SharedHttpStream open so the LiveStreamFiles
            // URL is accessible for the byte-check probe after PlaybackInfo completes.
            res.writeHead(200, { 'Content-Type': 'video/mp2t' });
            let streaming = true;
            let offset = 0;
            const chunkSize = 1880;
            function sendChunk() {
              if (!streaming) return;
              const chunk = TS_LOOP_BODY.subarray(offset, offset + chunkSize);
              offset = (offset + chunk.length) % TS_LOOP_BODY.length;
              const flushed = res.write(chunk.length > 0 ? chunk : TS_LOOP_BODY.subarray(0, chunkSize));
              if (flushed) {
                setTimeout(sendChunk, 10);
              } else {
                res.once('drain', sendChunk);
              }
            }
            req.on('close', () => { streaming = false; channelConnClose(url); });
            req.on('error', () => { streaming = false; channelConnClose(url); });
            sendChunk();
            return;
          }

          res.writeHead(404, { 'Content-Type': 'text/plain' });
          res.end('not found');
        });

        server.listen(assignedPort, '127.0.0.1', () => {
          resolve({
            url: baseUrl,
            close() {
              return new Promise((res) => server.close(res));
            },
          });
        });
        server.on('error', reject);
      });
    });
    tempServer.on('error', reject);
  });
}

if (require.main === module) {
  const port = Number.parseInt(process.argv[2] || process.env.HDHOMERUN_SIM_PORT || '0', 10) || 0;
  start(port).then(({ url, close }) => {
    console.log(`HDHomeRun simulator listening at ${url}`);
    console.log(`  GET ${url}/discover.json`);
    console.log(`  GET ${url}/lineup.json`);
    console.log(`  GET ${url}/auto/v4.1`);
    process.on('SIGTERM', () => close().then(() => process.exit(0)));
    process.on('SIGINT', () => close().then(() => process.exit(0)));
  }).catch((error) => {
    console.error('Failed to start HDHomeRun simulator:', error);
    process.exit(1);
  });
}

module.exports = { start };
