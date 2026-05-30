# Spec — E2 Live TV: HLS / TRANSCODE (conectar canal HDHomeRun al pipeline HLS existente)

Estado: in-progress. Plan: 0030 (subgate tras sharing/refcount). Fuente de verdad: /home/cdmonio/dev/jellyfin.
Objetivo: un canal Live TV HDHomeRun (hdhr_*) de Jellyrin se sirve vía HLS transcodeado reusando la infra HLS/transcode VOD existente: master.m3u8 + media playlist LIVE (sin EXT-X-ENDLIST mientras vive) + >=1 segmento .ts reproducible (video/mp2t, bytes>0) generado por ffmpeg desde el stream del simulador, con tracking (ActiveEncodings) y cleanup (matar ffmpeg al parar, cero huérfanos). Golden valida ambos targets (8097 + upstream fresco) contra el mismo simulador.

## Contrato upstream (citas)
- Live HLS: GET /Videos/{itemId}/live.m3u8 (DynamicHlsController.cs:165-345, GetLiveHlsStream) arranca ffmpeg con IsLiveOutput=true (:318), WaitForMinimumSegmentCount (:329), devuelve playlist via HlsHelpers.GetLivePlaylistText (:342).
- Playlist live = ffmpeg -hls_playlist_type event -hls_list_size 0 (DynamicHlsController.cs:1590, args :1635-1651). GetLivePlaylistText (HlsHelpers.cs:112-131) NO inyecta ni borra EXT-X-ENDLIST. Observable comparable: media playlist NO contiene #EXT-X-ENDLIST mientras el encoder vive. Sin delete_segments.
- Tracking/cleanup: _activeTranscodingJobs (TranscodeManager.cs:48); KillTranscodingJobs (:194-216, expuesto por DELETE /Videos/ActiveEncodings); KillTranscodingJob (:218-247) Cancel + job.Stop() (mata ffmpeg) + CloseLiveStreamIfNeeded si IsLiveOutput. Idle ping-timeout 60s (:153-191).

## Infra Jellyrin a reusar (citas)
- jellyrin-transcode/src/lib.rs: spawn_transcode_process, TranscodeProcess::{stop,wait,process_id} kill_on_drop (:100-239), HlsTranscodeLayout (:60-98), render_hls_master_playlist (:123-141), wait_for_hls_readiness (:167-187), HLS_MEDIA_PLAYLIST_NAME="main.m3u8".
- jellyrin-core build_hls_ffmpeg_command (:179-265): YA usa -f hls -hls_time -hls_playlist_type event -hls_segment_filename, -i input_path (acepta cualquier string -> URL HTTP ok). PARIDAD live ya presente.
- jellyrin-api: playback_transcode_info_response (:15887-15974), spawn_hls_transcode_task (:16080-16184) lanza ffmpeg, persiste running, registra stop_tx en TRANSCODE_STOPS, en stop process.stop()+cleanup_hls_transcode_files. active_encodings/stop_active_encoding (:21151-21210). Rutas /HlsSegment/Videos/{item_id}/{master.m3u8,main.m3u8,live.m3u8}, segmentos hls1/{playlist_id}/{seg} (:1146-1233). live.m3u8 YA existe (->hls_media_playlist).
- GAP: active_hls_transcode_session_for (:19317-19347) hace parse_jellyfin_uuid(item_id) y exige session.item.id==requested (MediaItem). hdhr_4.1 no es UUID -> rechazado. live_tv_channel_media_source (:10687-10734) hoy SupportsTranscoding:false.

## Decisiones (documentar en 0030)
- D1 punto de entrada item live: branch Live TV que NO pasa por media_item_by_id. Preferida D1a: branch en active_hls_transcode_session_for/playback_info_response que cuando item_id es hdhr_* (o live_tv_channel_by_id resuelve) resuelve canal, construye HlsTranscodeRequest input_path=URL canal, claim+spawn reusando helpers, con item_id sintético estable; relajar la comparación session.item.id==requested SOLO para sesiones live (test debe cubrir que no abre bypass en VOD). Alternativa D1b handler dedicado. Dev elige y documenta.
- D2 playlist live = event sin ENDLIST (Jellyrin ya emite -hls_playlist_type event). Aserción comparable: media playlist sin #EXT-X-ENDLIST mientras vive. Sin sliding window/delete_segments.
- D3 input ffmpeg = -i <URL /auto/vN> directa (1 conexión por encoding; verificable /stats maxConcurrent==1). No reusa el broadcast del proxy directo-TS. Comparable a SharedHttpStream (1 conexión por tuner stream).
- D4 cleanup: la TranscodeSession live se registra en TRANSCODE_STOPS (como VOD :16086-16089) para que DELETE /HlsSegment/Videos/ActiveEncodings?PlaySessionId= mate ffmpeg y borre sesión.
- D5 simulador SIN cambios (ya sirve MPEG-2 TS real en loop + /stats + /stats/reset).
- D6 sin idle-kill timer en Jellyrin (solo kill explícito); divergencia R-IDLE documentada.

## Invariantes
upstreamComparable: liveTvHdhrHlsMaster200 (master 200 m3u8 con #EXT-X-STREAM-INF), liveTvHdhrHlsMediaLive (media playlist 200, >=1 #EXTINF, SIN #EXT-X-ENDLIST), liveTvHdhrHlsSegment200 (segmento .ts 200 video/mp2t bytes>0), liveTvHdhrHlsActiveEncoding (listado en ActiveEncodings durante playback; ausente tras DELETE/Close en timeout bounded).
jellyrinOnly: liveTvHdhrHlsTranscodeUrl (MediaSource canal con SupportsTranscoding:true + TranscodingUrl live.m3u8/master.m3u8), liveTvHdhrHlsFfmpegReaped (/stats currentConcurrent[/auto/vN]===0 tras DELETE, no-huérfanos; upstream degrada a observación honesta si refill R8, no fuerza gate).
Gate del subgate se decide por los 4 comparables.

## Áreas afectadas (cerrada)
Rust: jellyrin-api/src/lib.rs (live_tv_channel_media_source 10687-10734 campos transcode; D1 branch en active_hls_transcode_session_for 19317-19347 y/o playback_info_response 15817-16016; verificar hls_master/media_playlist_response_for 17112-17200 no inyecta ENDLIST). jellyrin-core build_hls_ffmpeg_command 179-265 SOLO si input HTTP infinito requiere flags mínimos (actualizar test). jellyrin-transcode sin cambios.
Golden: browser-trace.js (invariantes ~322; extender runLiveTvFlow HDHomeRun block ~3678-3796 o flow live-tv-hls; reusar browserFetchText/firstPlaylistUri/browserFetchStreamProbe; DELETE ActiveEncodings + lectura ActiveEncodings + /stats). livetv-real.js (4 comparables + 2 jellyrinOnly, coverage, evidence documentando lo no comparable).
Simulador: sin cambios salvo fallo de ingest.

## Criterios de aceptación (binarios)
Build/unit: cargo fmt --all -- --check; cargo build -p jellyrin-api -p jellyrin-core -p jellyrin-transcode; cargo test (sin regresión). Test: HlsTranscodeRequest live -> comando con -hls_playlist_type event y -i <url> sin delete_segments. Test: live_tv_channel_media_source remoto incluye SupportsTranscoding:true, TranscodingSubProtocol:"hls", TranscodingUrl con live.m3u8/master.m3u8. Test: sesión live por canal hdhr_* no requiere parse_jellyfin_uuid del canal.
E2E (ambos targets, mismo simulador): node qa/golden/livetv-real.js -> status upstream-validated, completedTargets jellyrin+upstream, coverage.complete, exit 0. comparison.json AMBOS: liveTvHdhrHlsMaster200, liveTvHdhrHlsMediaLive (sin ENDLIST), liveTvHdhrHlsSegment200, liveTvHdhrHlsActiveEncoding = true. jellyrin: liveTvHdhrHlsTranscodeUrl, liveTvHdhrHlsFfmpegReaped = true. comparison.failed=false.
Cleanup/no-huérfanos: tras DELETE ActiveEncodings (Jellyrin), GET ActiveEncodings ya no lista la sesión (poll <=5s) Y /stats currentConcurrent[/auto/vN]===0 (poll <=5s); el dir de sesión HLS no existe tras stop.
Alcance: git diff --stat crates/ <=2 (lib.rs + opc jellyrin-core); qa/golden/ <=2 (browser-trace.js + livetv-real.js); simulador sin tocar salvo ingest (entonces +1 documentado).

## Riesgos
R-INGEST (ffmpeg HTTP infinito; wait_for_hls_readiness timeout 5s puede no bastar para 1er segmento -> subir SOLO para live, documentar). R-IDLE (sin idle-kill, fuera de gate). R8 heredado (refill upstream -> reaped degradado honesto). R-D1 (relajar item_id solo live, no bypass VOD; test). R-SHARE (ingest ffmpeg abre conexión propia; reset /stats entre bloques, no romper subgate sharing).

## Fuera de alcance
Recording real, restart recovery, idle-kill 60s, TunerCount, UDP discovery, ABR multi-bitrate, delete_segments sliding-window, LiveStreams/Open/Close con LiveStreamId, fMP4.

## ADDENDUM — liveTvHdhrHlsActiveEncoding: degradación honesta upstream (evidencia empírica)

Estado: jellyrinOnly. La degradación es genuina y verificada, NO una omisión sin evidencia.

Evidencia directa (curl contra upstream 8098):
```
curl -X GET http://127.0.0.1:8098/Videos/ActiveEncodings \
  -H "X-MediaBrowser-Token: <token>"
HTTP 405 Method Not Allowed
Allow: DELETE
```

upstream Jellyfin sólo expone DELETE /Videos/ActiveEncodings (TranscodeManager.cs KillTranscodingJobs). El endpoint GET no existe en la API pública de Jellyfin. El cleanup/kill se valida en upstream vía DELETE (que sí soporta y retorna 2xx cuando PlaySessionId + DeviceId son correctos).

Jellyrin expone GET /Videos/ActiveEncodings (retorna array JSON con sesiones activas incluyendo PlaySessionId, ItemId) y DELETE /HlsSegment/Videos/ActiveEncodings (mata ffmpeg + borra sesión). Ambos son comparables funcionalmente pero el GET es una extensión Jellyrin.

Esta degradación es permanente para el upstream actual y no requiere acción adicional.
