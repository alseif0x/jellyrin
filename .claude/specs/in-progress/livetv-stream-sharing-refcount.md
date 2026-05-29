# Spec — E2 Live TV subgate: STREAM SHARING / REFCOUNT (2 clientes + cleanup)

Estado: in-progress. Plan: plans/0030 (subgate tras livetv-hdhomerun-real). Fuente de verdad: /home/cdmonio/dev/jellyfin.
Decisión pregunta abierta: KEYEAR el registro de sharing por la URL resuelta del stream (/auto/vN) — verificado seguro (cada canal hdhr_* resuelve a URL distinta). No requiere tocar firma de live_tv_stream_file salvo para propagar la URL ya disponible.

## Objetivo
Jellyrin comparte UNA conexión saliente al simulador HDHomeRun entre 2 clientes concurrentes del MISMO canal (refcount), y la libera cuando ambos cierran (cero conexiones huérfanas). Validado contra Jellyrin 8097 + upstream fresco, mismo simulador, métrica observable comparable.

## Contrato upstream verificado (citas)
- SharedHttpStream.Open (src/Jellyfin.LiveTv/TunerHosts/SharedHttpStream.cs:44-75): UNA conexión al tuner (ResponseHeadersRead 58-60) -> copia a temp file .ts (StartStreaming 90-106); MediaSource.Path -> /LiveTv/LiveStreamFiles/{UniqueId}/stream.ts (66). N consumidores leen el MISMO temp file (LiveStream.GetStream FileShare.ReadWrite, LiveStream.cs:97-114). Sharing upstream = 1 conexión saliente + N lectores, NO N conexiones.
- Refcount en MediaSourceManager (_openStreams ConcurrentDictionary keyed by LiveStreamId; MediaSourceManager.cs:60,547-571). Reuso+inc: DefaultLiveTvService.GetChannelStreamWithDirectStreamProvider (DefaultLiveTvService.cs:462-483): si existe stream con OriginalStreamId==streamId y EnableStreamSharing -> ConsumerCount++ y devuelve el existente SIN abrir nueva conexión. Cierre+dec+liberación: CloseLiveStream (MediaSourceManager.cs:909-932): ConsumerCount--; a <=0 TryRemove + Close() (cancela token -> detiene copia y cierra conexión saliente).
- TunerCount/LiveTvConflictException (HdHomerunHost.cs:385-398): se evalúa por número de live streams DISTINTOS (canales distintos), NO por consumidores. 2 consumidores del MISMO canal = 1 stream = NO dispara conflicto. -> FUERA de este subgate (D-TUNERLIMIT).
- API observable: POST /LiveTv/LiveStreams/Open (OpenLiveStreamDto -> LiveStreamResponse, MediaInfoController.cs:269-311), POST /LiveTv/LiveStreams/Close?liveStreamId= (314-318), PlaybackInfo AutoOpenLiveStream (116-227), GET /LiveTv/LiveStreamFiles/{streamId}/stream.{container} (LiveTvController.cs:1147-1163).

## Estado Jellyrin (gap)
proxy_live_tv_channel_url (lib.rs:10297-10317): HttpClient::new() + get por CADA request, sin registro/refcount/LiveStreamId. 2 GET concurrentes = 2 conexiones al simulador. stream_live_tv_channel (10285-10295), live_tv_stream_file (10186-10216). AppState (81-86) sin estado de refcount.

## Decisión D-SHARE (métrica observable comparable)
El simulador cuenta conexiones concurrentes entrantes por path /auto/vN y expone GET /stats {maxConcurrentByChannel, currentConcurrentByChannel} + POST /stats/reset. Con sharing: 2 clientes mismo canal -> maxConcurrent==1. Sin sharing: ==2. Idéntico en ambos targets (ambos consumen el simulador por HTTP); NO depende de internals. POST /stats/reset ANTES de cada bloque por target (estado global compartido).

## Invariantes
upstreamComparable: liveTvHdhrTwoClientStream (2 clientes mismo canal -> maxConcurrentByChannel[canal]===1); liveTvHdhrStreamRefcountReleased (tras cerrar ambos, currentConcurrentByChannel[canal]===0 dentro de timeout bounded).
jellyrinOnly: liveTvHdhrTwoClientByteCheck (2º consumidor Jellyrin recibe video/mp2t byteLength>=1 vía probe AbortController).
Gate del subgate se decide por los 2 comparables (misma señal externa en ambos). Reusar split del addendum it.2 de livetv-hdhomerun-real.

## Áreas afectadas (cerrada)
Rust lib.rs: AppState (81-86) registro Arc<Mutex<HashMap<url, SharedLiveStreamHandle{refcount, productor}>>>; proxy_live_tv_channel_url (10297) refactor a 1 productor + fan-out a N consumidores (NO re-bufferizar el body entero), refcount inc en open / dec en cierre-o-abort (guard RAII), a 0 cancela productor y cierra conexión saliente; stream_live_tv_channel (10285) propaga la URL/clave. Tests unit: (a) 2 consumidores mismo canal -> 1 conexión saliente; (b) cerrar ambos -> handle eliminado + conexión cancelada; (c) canales distintos NO comparten.
Golden: hdhomerun-sim.js contador concurrente por canal + GET /stats + POST /stats/reset. browser-trace.js bloque HDHomeRun (~3686-3787): reset, 2 probes concurrentes solapadas del MISMO canal, leer /stats -> twoClientStream; cerrar, drain bounded, /stats -> refcountReleased; jellyrin byteCheck. Declarar invariantes (~3322). livetv-real.js: añadir a upstreamComparable/jellyrinOnly, coverage, evidence.

## Criterios de aceptación (binarios)
A Simulador: GET /stats 200 con maxConcurrentByChannel/currentConcurrentByChannel; POST /stats/reset 200 limpia; 1 GET vivo -> current[/auto/v4.1]===1, al cerrar ->0; diff = solo hdhomerun-sim.js.
B Rust: cargo fmt/build/test verdes; test "2 consumidores mismo canal -> 1 conexión saliente"; test "cerrar ambos -> handle eliminado + conexión cancelada (refcount 0)"; test "2 canales distintos NO comparten"; diff crates/ <=1 archivo (lib.rs).
C Golden E2E: invariantes en sus sets; reset+2 probes concurrentes por target; node livetv-real.js (8097+upstream) -> upstream-validated, completedTargets jellyrin+upstream, coverage.complete, comparison.failed=false, exit 0; comparison.json ambos targets twoClientStream==true (maxConcurrent[canal]===1) y refcountReleased==true (current[canal]===0); jellyrin byteCheck==true; sin tuners/sim residuales (GET /stats final 0); diff qa/golden = 3 archivos.

## Riesgos / decisiones (0030)
R7 fan-out 1->N + cancelación a refcount 0 (productor huérfano si consumidor aborta sin decrementar) -> guard que decremente en drop + test de release. R8 upstream puede mantener conexión para refill; si current no baja a 0 en timeout, degradar refcountReleased a aserción honesta comparable y DOCUMENTAR (no marcar validado lo no comparado). R9 las 2 probes deben SOLAPARSE temporalmente (no abort inmediato) para capturar maxConcurrent. R10 reset entre targets obligatorio. D-SHARE: métrica = conexiones al simulador. D-TUNERLIMIT: TunerCount fuera de este subgate.

## Fuera de alcance
HLS/transcode, recording real, restart recovery, UDP discovery, tuner legacy, migrar Jellyrin a LiveStreams/Open/Close con LiveStreamId, límite TunerCount multi-canal.
