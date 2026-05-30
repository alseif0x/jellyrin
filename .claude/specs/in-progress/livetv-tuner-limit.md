# Spec — E2 Live TV: LÍMITE TunerCount (conflicto multi-canal)

Estado: in-progress. Plan: 0030 (tras recording-real). Fuente de verdad: /home/cdmonio/dev/jellyfin. Barra: 0 gaps, 0 atajos, comparación genuina contra upstream.
Objetivo: Jellyrin rechaza abrir un stream de canal cuando el nº de live streams DISTINTOS activos del mismo tuner host alcanza su TunerCount, con error observable comparable a upstream. 2 del MISMO canal comparten (no cuentan doble). Tras cerrar uno, se puede abrir otro (recuperación). Golden compara N ok / N+1 conflicto / recuperación en Jellyrin 8097 + upstream 8098, mismo simulador con TunerCount=1.

## Contrato upstream (citas)
- HdHomerunHost.GetChannelStream (TunerHosts/HdHomerun/HdHomerunHost.cs:385-398): si TunerCount>0 y currentLiveStreams.Where(TunerHostId==host.Id).Count() >= TunerCount -> throw LiveTvConflictException. currentLiveStreams = _openStreams.Values (MediaSourceManager.cs:556, OpenLiveStreamInternal :547).
- Semántica: mismo canal comparte -> ConsumerCount++ SIN nueva entrada en _openStreams (DefaultLiveTvService.cs:462-477) -> NO cuenta doble. Canales distintos -> entradas distintas -> cuentan. Liberación en CloseLiveStream (MediaSourceManager.cs:909-931): ConsumerCount--; a <=0 TryRemove+Close.
- TunerCount: TunerHostInfo.TunerCount (Model/LiveTv/TunerHostInfo.cs:42) leído de discover.json DiscoverResponse.TunerCount (DiscoverResponse.cs:25); fijado en HdHomerunHost.TryGetTunerHostInfo (:547).
- PROPAGACIÓN HTTP (CRÍTICO): LiveTvConflictException NO se captura en la ruta real (DefaultLiveTvService.GetChannelStreamWithDirectStreamProvider llama hostInstance.GetChannelStream directo, solo captura FileNotFound/OperationCanceled). No hay catch en Jellyfin.Api. ExceptionMiddleware.GetStatusCode (Middleware/ExceptionMiddleware.cs:123-135) NO incluye LiveTvConflictException -> cae en _ => 500. => UPSTREAM DEVUELVE HTTP 500 text/plain. El conflicto se dispara en POST /Items/{id}/PlaybackInfo (AutoOpenLiveStream=true) / POST /LiveStreams/Open (momento de OPEN, no la GET de bytes).

## Estado Jellyrin (citas)
- LIVE_STREAM_REGISTRY (lib.rs:84) keyed por URL del canal; SharedLiveStreamHandle (124-132) refcount/generation/sender/_cancel. SIN TunerHostId ni límite.
- proxy_live_tv_channel_url (10403-10542): comparte por URL, no consulta TunerHostId/TunerCount, no aplica límite.
- stream_live_tv_channel (10360-10370): si remoto -> proxy. Llamado desde live_tv_stream_file (10261-10290, ruta /LiveTv/LiveStreamFiles/{id}/{container}).
- DIVERGENCIA: canales hdhr_ NO pasan por PlaybackInfo en Jellyrin (browser-trace.js:3737 "PlaybackInfo 400 for hdhr_"); el stream se abre por GET /LiveTv/LiveStreamFiles/{id}/stream.ts -> proxy. Único punto de enforcement en Jellyrin = proxy_live_tv_channel_url / stream_live_tv_channel (momento de GET).
- Canal materializado lleva TunerHostId (lib.rs:10710) y el objeto channel está en stream_live_tv_channel. TunerCount ya se persiste desde discover.json en add_live_tv_tuner_host->live_tv_hdhomerun_channels_from_payload (9561-9566) en config livetv.TunerHosts[].

## Decisiones (0030)
D1 TunerCount=1 para el test (con el simulador: 4.1 y 5.1 no-DRM; N=1 -> abrir 4.1 ok, 5.1 conflicto, cerrar 4.1, 5.1 ok). Leído de discover.json del simulador (paridad). Configurable HDHOMERUN_SIM_TUNER_COUNT=1; default fichero 4 (no romper otros subgates).
D2 Status del conflicto = HTTP 500 en ambos (verdad observable de upstream via ExceptionMiddleware _=>500). Jellyrin imita 500. R-CONFLICT-500: es bug latente de upstream (nombre sugiere 409) pero comparamos contra la verdad; el golden compara vs upstream fresco, no hardcode.
D3 Contar por tuner host: añadir tuner_host_id a SharedLiveStreamHandle; al abrir una URL NUEVA contar entradas del registro con mismo tuner_host_id; URL ya presente (2º consumidor) NO incrementa (solo refcount++) -> preserva sharing. Chequeo+insert ATÓMICO bajo el lock del registro (evitar TOCTOU).
D4 Golden mantiene N solapados con browserFetchStreamProbeOverlap (holdMs>=600). N+1 conflicto: verificar status 500. Recuperación: cerrar probe de A + esperar /stats currentConcurrent[/auto/v4.1]===0 antes de reintentar B.
D5 TunerCount cuenta solo streams DIRECTOS en este subgate (HLS LIVE_HLS_SESSIONS y recording LIVE_TV_RECORDING_REGISTRY tienen registros separados; unificar es subgate futuro). Divergencia R-LIMIT-SCOPE documentada; el golden solo ejercita streams directos.

## Invariantes
upstreamComparable: liveTvHdhrTunerLimitFirstOpen (abrir 4.1 ok, 200 + >=1 byte, ambos); liveTvHdhrTunerLimitConflict (con 4.1 abierto, abrir 5.1 distinto mismo tuner -> HTTP 500; upstream via POST /Items/{5.1}/PlaybackInfo?AutoOpenLiveStream=true; Jellyrin via GET /LiveTv/LiveStreamFiles/{5.1}/stream.ts con 4.1 mantenido por overlap; verificar status EXACTO 500); liveTvHdhrTunerLimitRecovery (tras cerrar 4.1 y drenar current===0, abrir 5.1 ok 200 + bytes).
jellyrinOnly: liveTvHdhrTunerLimitSharingExempt (2 consumidores del MISMO canal 4.1 con TunerCount=1 NO disparan conflicto; maxConcurrent[/auto/v4.1]===1 y ambos reciben bytes). (Promover a comparable solo si se demuestra probe upstream equivalente.)

## Áreas afectadas (cerrada)
Rust lib.rs: SharedLiveStreamHandle (~124-132) +tuner_host_id:Option<String>; proxy_live_tv_channel_url (~10403) firma +tuner_host_id/+tuner_count, chequeo de límite en Phase 1 ANTES de insertar URL nueva (bajo el lock), Err->HTTP 500 (ApiError::internal mapea 500); stream_live_tv_channel (~10360) extrae TunerHostId del channel y resuelve TunerCount del tuner host en config livetv; helper tuner_host_id->tuner_count; tests unit junto a sharing (~49401): (a) mismo canal TunerCount=1 2º consumidor ok; (b) 2 canales distintos 2º Err; (c) tras drop guard del 1º, 2º ok.
Simulador hdhomerun-sim.js: buildDiscoverResponse (~129-141) TunerCount configurable env HDHOMERUN_SIM_TUNER_COUNT default 4.
Golden browser-trace.js: bloque tuner-limit en runLiveTvFlow (bloque HDHomeRun ~3839-3920); invariantes en init (~329) y label map (~7810). livetv-real.js: 3 comparables + 1 jellyrinOnly, coverage/evidence.

## Criterios (binarios)
- cargo fmt --check; cargo clippy --workspace --all-targets (sin warnings nuevos); cargo test --workspace.
- Test: mismo canal TunerCount=1 -> 2º open NO Err (sharing exento). Test: 2 canales distintos TunerCount=1 -> 2º open Err. Test: tras drop refcount del A, B ok (recuperación).
- HDHOMERUN_SIM_TUNER_COUNT=1 node hdhomerun-sim.js -> /discover.json "TunerCount":1.
- Golden (8097+8098, sim TunerCount=1): comparison.json AMBOS liveTvHdhrTunerLimitFirstOpen/Conflict/Recovery=true; el conflicto verifica status EXACTO 500 en ambos; jellyrin liveTvHdhrTunerLimitSharingExempt=true; coverage.complete; status upstream-validated; comparison.failed=false; exit 0.
- Cleanup: /stats currentConcurrent de 4.1 y 5.1 vuelve a 0 (sin huérfanos). git diff --stat <=5 ficheros; sin ficheros nuevos fuera de scope.

## Riesgos
R-CONFLICT-500 (upstream 500 no 409; comparar vs upstream fresco). R-ENFORCE-POINT (Jellyrin enforce en GET byte-stream, upstream en open; mismo 500 observable, distinto endpoint, divergencia aceptable). R-LIMIT-SCOPE (solo streams directos; HLS/recording fuera). R-TOCTOU (chequeo+insert bajo lock). R-RECOVERY-TIMING (poll /stats===0 antes de reintentar). R-NON-DRM-CHANNELS (simulador solo 4.1/5.1 no-DRM -> N=1).

## Fuera de alcance
restart recovery, UDP discovery, tuner legacy, series-timer real, límite M3UTunerHost, unificar HLS/recording en el contador, "arreglar" 500-vs-409 de upstream.
