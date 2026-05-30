# Spec — E2 Live TV: GRABACIÓN REAL A FICHERO (timer -> recording reproducible)

Estado: in-progress. Plan: 0030 (subgate tras livetv-hls-transcode). Fuente de verdad: /home/cdmonio/dev/jellyfin. Barra: 0 gaps, 0 atajos, comparación GENUINA por bytes reales (ffprobe) en AMBOS targets.
Objetivo: un timer sobre canal HDHomeRun (hdhr_*) dispara grabación REAL: Jellyrin abre el stream del canal y escribe un .ts real durante la ventana; al terminar, aparece en GET /LiveTv/Recordings como Completed con fichero REPRODUCIBLE (bytes video reales, no fixture), reproducible por su stream. Validado vs Jellyrin 8097 + upstream 8098, mismo simulador, verificado por ffprobe en ambos.

## Contrato upstream (citas)
- Trigger: TimerManager.AddOrUpdateSystemTimer (Timers/TimerManager.cs:89-109): si startDate(=StartDate-PrePadding) < now -> TimerFired INMEDIATO (:101-104); si no programa System.Threading.Timer. DefaultLiveTvService.CreateTimer (:215-261) Id=Guid, IsManual, Add. OnTimerManagerTimerFired (:540-596): recordingEndDate=EndDate+PostPadding; si <=now borra y sale; RecordStream(activeRec, channel, recordingEndDate) (:586).
- RecordingsManager.RecordStream (Recordings/RecordingsManager.cs:304-425): media sources del canal, abre live stream si RequiresOpening, GetRecorder (:337), recordingPath+unicidad, duration=recordingEndDate-now, recorder.Record(...). OnStarted (:349-362): Path, _activeRecordings, timer.Status=InProgress+AddOrUpdate. Al terminar: Status=Completed, cierra stream, borra fichero si 0 bytes (:398), si File.Exists -> timer.RecordingPath+Status=Completed (:414-419); si no, borra timer.
- GetRecorder (:791-801): DirectRecorder (COPY) si Container termina en "ts" Y Protocol==File||Http Y !RequiresLooping. Canal HDHomeRun (Http, ts) -> DirectRecorder COPY, NO transcode.
- DirectRecorder.RecordFromMediaSource (IO/DirectRecorder.cs:82-111): GET(mediaSource.Path, ResponseHeadersRead) + FileStream(CreateNew) + onStarted + CancellationTokenSource(duration) linked + CopyUntilCancelled. Sin ffmpeg.
- Naming: GetRecordingPath (:476-585) base=RecordingPath o DataPath/livetv/recordings; fichero=GetValidFilename(RecordingHelper.GetRecordingName(timer))+".ts". GetRecordingName (RecordingHelper.cs:10-69) no-serie -> name+" "+StartDate.ToLocalTime("yyyy_MM_dd_HH_mm_ss"). Ext siempre .ts.
- Endpoints: GET /LiveTv/Recordings (LiveTvController.cs:268-293) -> items de LIBRERIA en recording folders (Status InProgress del ActiveRecordingInfo). GET /LiveTv/Recordings/{id} (:406-427) BaseItemDto. GET /LiveTv/LiveRecordings/{id}/stream (:1120-1134) SOLO InProgress (temp file que se escribe). Completed se reproduce como item normal. DELETE /LiveTv/Recordings/{id} (:778-796) DeleteItem(DeleteFileLocation=false).
- Test rápido: POST /LiveTv/Timers StartDate≈now, EndDate≈now+Ns, PrePadding=0 PostPadding=0 -> dispara inmediato, graba ~Ns, completa.

## Estado Jellyrin (citas) — FIXTURE vs REAL
FIXTURE (sustituir para invariantes nuevos): live_tv_recording_items (lib.rs:10953-10984) lee config["Recordings"]; live_tv_recording_item (:10986-11041); stream_live_tv_recording (:10333-10340) sirve fixture (.ts placeholder texto); create_live_tv_timer (:11249-11257)->upsert_live_tv_timer (:11344-11382) SOLO persiste en config["Timers"], SIN scheduler/grabación.
REAL a reusar: proxy_live_tv_channel_url (:10385-~10490) patrón HttpClient.get(url).bytes_stream() para leer el stream; live_tv_channel_by_id (:10135), live_tv_channel_path (:10713), live_tv_channel_is_remote (:10721); HttpClient=reqwest::Client (:49); AppState (:121-126) SIN estado de grabaciones -> añadir static OnceLock<Mutex<HashMap>> análogo a LIVE_STREAM_REGISTRY (:84)/LIVE_HLS_SESSIONS (:99); patrón tokio::spawn+oneshot+cancel ya usado en proxy/HLS.

## Decisiones (0030)
D1 Trigger fire-on-create SÍNCRONO: en upsert/create timer (key Timers, no series) si StartDate<=now y EndDate+PostPadding>now -> tokio::spawn tarea grabación. Timers futuros ya cubiertos por el scheduler persistente E2.12 (`run_due_live_tv_timers` + task periódico del server).
D2 Recorder=COPY directo (no transcode, no ffmpeg): GET stream + copy de bytes a FileStream hasta duration. Container .ts. Reusa HttpClient.get(url).bytes_stream().
D3 Duración test 4s (env JELLYRIN_LIVETV_RECORD_SECS default 4), Pre/PostPadding=0; golden poll Completed bounded (timeout env default 30s, intervalo 1s).
D4 Almacenamiento: al completar persistir en config["Recordings"] un recording REAL {Id (distinto del fixture), Name, ChannelId, Path(fichero .ts real), Status:"Completed", StartDate, EndDate, DateCreated}. Path: DataPath/livetv/recordings/<ValidFilename(Name+" "+StartDate "yyyy_MM_dd_HH_mm_ss")>.ts (paridad naming).
D5 Post-scan a librería FUERA (recomendado NO incluir; Jellyrin proyecta desde config["Recordings"]; observable comparable no lo requiere). Divergencia R-LIBSCAN.
D6 InProgress vs Completed: durante graba Status InProgress + stream parcial real por /LiveTv/LiveRecordings/{id}/stream; gate valida estado FINAL Completed+bytes (más determinista).
D7 Verificación por ffprobe: golden descarga el stream/fichero del recording en cada target y ffprobe exige >=1 paquete de stream video. EN AMBOS. Esta es la comparación genuina (no header/ext, no PlaybackInfo-only).
D8 Transición fixture: liveTvRecordings200/liveTvRecordingStream200 (jellyrinOnly, bloque sintético if jellyrin) se MANTIENEN sin cambio; los nuevos invariantes reales son adicionales, para AMBOS, en el bloque HDHomeRun; Id del recording real distinto del fixture (sin colisión).

## Invariantes
upstreamComparable (gate): liveTvHdhrTimerRecordingCreated (POST Timer 200 con Id, ambos); liveTvHdhrRecordingCompleted (poll bounded GET Recordings/Timers hasta recording del canal Status Completed, ambos); liveTvHdhrRecordingPlayable (descargar fichero/stream del recording Completed -> ffprobe >=1 paquete video, ambos -> bytes reales).
jellyrinOnly: liveTvHdhrRecordingCleanup (/stats currentConcurrent[/auto/vN]===0 tras completar + DELETE recording 204 + ausente; degradar parte /stats si refill upstream R8). Fixture previos sin cambio.

## Áreas afectadas (cerrada)
Rust lib.rs (1 archivo): AppState/static registro grabaciones activas (handle cancel+path); upsert/create timer rama StartDate<=now -> spawn; NUEVA fn record_channel_to_file (GET stream bytes_stream, tokio::fs::File CreateNew, copy hasta duration via timeout/select+cancel, naming paridad, persistir InProgress->Completed o borrar si 0 bytes, limpiar conexión saliente sin huérfanos); live_tv_recording_stream (:10232)/stream_live_tv_recording (:10333) sirven el fichero REAL (verificar live_tv_recording_path :11043); tests: naming, copy bytes a fichero (mock TS), StartDate<=now graba / futuro no, recording Completed persistido con Path+Status, 0-bytes no crea Completed.
Golden (<=2 + sim sin tocar): browser-trace.js invariantes (~323) + bloque HDHomeRun (tras liveTvHdhrChannelMatched ~3702, AMBOS targets): POST Timer corto -> poll Recordings Completed (env JELLYRIN_LIVETV_RECORD_POLL_TIMEOUT_MS/_INTERVAL_MS) -> descargar fichero -> ffprobe (spawn, ffprobe -show_packets -select_streams v) assert >=1 paquete video -> (jellyrin) /stats===0 + DELETE recording. Reusar browserFetchBinary/nodeHttpJson. livetv-real.js: 3 comparables a upstreamComparable + cleanup a jellyrinOnly; coverage/evidence. Simulador SIN cambios.

## Criterios (binarios)
A: cargo fmt/clippy/build/test verdes; tests naming, copy-bytes, trigger StartDate<=now vs futuro, Completed persistido Path+Status, 0-bytes no Completed; diff crates/ <=1.
B: node qa/golden/livetv-real.js (8097+8098) -> status upstream-validated, completedTargets ambos, coverage.complete, exit 0; comparison.json AMBOS liveTvHdhrTimerRecordingCreated/RecordingCompleted/RecordingPlayable=true; RecordingPlayable por ffprobe >=1 paquete video en AMBOS; jellyrin liveTvHdhrRecordingCleanup=true; comparison.failed=false; fixture liveTvRecordings200/RecordingStream200 siguen true en jellyrin; run no cuelga (ventana <=4s, poll <=30s).
C cleanup: cero ffmpeg/copy huérfanos; /stats final 0; ficheros solo en DataPath/livetv/recordings/; recording borrado limpia config["Recordings"].
D alcance: diff qa/golden <=2; sim sin tocar; sin ficheros fuera de scope.

## Riesgos
R-LIBSCAN, R-TRIGGER (solo StartDate≈now), R8 refill (/stats cleanup jellyrinOnly; reset /stats antes del bloque), R-DETERMINISM (throttle ~13kB/s -> ~52KB/4s suficiente ffprobe; subir RECORD_SECS si hace falta, no degradar aserción), R-INPROGRESS (gate valida Completed final), R-RACE (guard cancel en drop, sin conexión huérfana).

## Fuera de alcance
Series-timer real (solo timer simple/manual; series fixture jellyrinOnly sin cambio), post-scan librería (D5), restart recovery, TunerCount, UDP discovery, tuner legacy, EncodedRecorder/transcode (copy es paridad), padding no-cero, retry/keep, post-processor, metadata.
